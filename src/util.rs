use chrono::{Local, NaiveDate, NaiveTime, TimeZone};
use directories::BaseDirs;
use regex::Regex;
use std::sync::LazyLock;

// ── Cached project directories ──────────────────────────────

static PROJECT_DIRS: LazyLock<Option<directories::ProjectDirs>> =
    LazyLock::new(|| directories::ProjectDirs::from("tech", "appachi", "suvadu"));

/// Cached project directory lookup. Avoids re-computing paths on every call.
/// Called from config, db, hooks, integrations, and alias modules.
pub fn project_dirs() -> Option<&'static directories::ProjectDirs> {
    PROJECT_DIRS.as_ref()
}

/// Threshold above which a timestamp is treated as microseconds (not milliseconds).
/// `9_999_999_999_999` is ~Nov 2286 in milliseconds, so any value above it is certainly
/// microseconds (16+ digits). Used consistently across all display and normalization code.
pub const MICROSECOND_THRESHOLD: i64 = 9_999_999_999_999;

/// Normalize a timestamp for display by converting microseconds to milliseconds.
/// This is the single function all display/formatting code should call.
/// Does NOT handle seconds→ms conversion (that's `normalize_timestamp` for ingestion).
pub const fn normalize_display_ms(ts: i64) -> i64 {
    if ts > MICROSECOND_THRESHOLD {
        ts / 1000
    } else {
        ts
    }
}

/// Parse a date string input into a Unix timestamp (milliseconds).
///
/// Supported formats:
/// - "YYYY-MM-DD" -> Returns timestamp at given `time_of_day`
/// - "today" -> Returns today at `time_of_day`
/// - "yesterday" -> Returns yesterday at `time_of_day`
///
/// `is_end_of_day`: If true, defaults to 23:59:59.999. If false, 00:00:00.000.
pub fn parse_date_input(input: &str, is_end_of_day: bool) -> Option<i64> {
    let input = input.trim().to_lowercase();

    let date = if input == "today" {
        Local::now().date_naive()
    } else if input == "yesterday" {
        Local::now().date_naive().pred_opt()?
    } else {
        NaiveDate::parse_from_str(&input, "%Y-%m-%d").ok()?
    };

    let time = if is_end_of_day {
        NaiveTime::from_hms_milli_opt(23, 59, 59, 999)?
    } else {
        NaiveTime::from_hms_milli_opt(0, 0, 0, 0)?
    };

    let dt = date.and_time(time);
    let dt_local = Local.from_local_datetime(&dt).single()?;

    Some(dt_local.timestamp_millis())
}

/// A pre-compiled exclusion pattern: either a valid regex or a literal substring.
pub enum CompiledExclusion {
    Regex(Regex),
    Substring(String),
}

/// Compile exclusion patterns once for reuse across multiple `is_excluded` calls.
/// Invalid regex patterns fall back to substring matching with a warning.
pub fn compile_exclusions(patterns: &[String]) -> Vec<CompiledExclusion> {
    patterns
        .iter()
        .map(|p| {
            Regex::new(p).map_or_else(
                |e| {
                    eprintln!("suvadu: invalid exclusion regex '{p}', using substring match: {e}");
                    CompiledExclusion::Substring(p.clone())
                },
                CompiledExclusion::Regex,
            )
        })
        .collect()
}

/// Check if a command matches any of the pre-compiled exclusion patterns.
pub fn is_excluded_compiled(command: &str, exclusions: &[CompiledExclusion]) -> bool {
    for pattern in exclusions {
        match pattern {
            CompiledExclusion::Regex(re) => {
                if re.is_match(command) {
                    return true;
                }
            }
            CompiledExclusion::Substring(s) => {
                if command.contains(s.as_str()) {
                    return true;
                }
            }
        }
    }
    false
}

/// Check if a command matches any of the exclusion patterns.
/// Patterns are treated as Regex first, falling back to substring match if invalid regex.
/// Convenience wrapper that compiles exclusions on each call.
#[cfg(test)]
pub fn is_excluded(command: &str, exclusions: &[String]) -> bool {
    let compiled = compile_exclusions(exclusions);
    is_excluded_compiled(command, &compiled)
}

/// RAII guard that sets up and tears down the terminal for TUI rendering.
/// On creation it enters raw mode and the alternate screen.
/// On drop (including panic unwind) it restores the terminal.
pub struct TerminalGuard {
    terminal: ratatui::Terminal<ratatui::backend::CrosstermBackend<std::io::Stdout>>,
}

impl TerminalGuard {
    /// Enter raw mode + alternate screen and return a ready terminal.
    pub fn new() -> Result<Self, Box<dyn std::error::Error>> {
        crossterm::terminal::enable_raw_mode()?;
        let mut stdout = std::io::stdout();
        crossterm::execute!(stdout, crossterm::terminal::EnterAlternateScreen)?;
        let backend = ratatui::backend::CrosstermBackend::new(stdout);
        let terminal = ratatui::Terminal::new(backend)?;
        Ok(Self { terminal })
    }

    /// Borrow the underlying terminal for rendering.
    pub const fn terminal(
        &mut self,
    ) -> &mut ratatui::Terminal<ratatui::backend::CrosstermBackend<std::io::Stdout>> {
        &mut self.terminal
    }
}

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        let _ = crossterm::terminal::disable_raw_mode();
        let _ = crossterm::execute!(
            self.terminal.backend_mut(),
            crossterm::terminal::LeaveAlternateScreen
        );
        let _ = self.terminal.show_cursor();
    }
}

/// RAII guard for stderr-based TUI (used by search, which needs stdout free for shell integration).
/// Restores terminal on drop, including panic unwind.
pub struct TerminalGuardStderr;

impl TerminalGuardStderr {
    /// Enter raw mode + alternate screen on stderr.
    pub fn new() -> Result<Self, Box<dyn std::error::Error>> {
        crossterm::terminal::enable_raw_mode()?;
        crossterm::execute!(std::io::stderr(), crossterm::terminal::EnterAlternateScreen)?;
        Ok(Self)
    }
}

impl Drop for TerminalGuardStderr {
    fn drop(&mut self) {
        let _ = crossterm::terminal::disable_raw_mode();
        let _ = crossterm::execute!(std::io::stderr(), crossterm::terminal::LeaveAlternateScreen);
    }
}

/// RAII guard for stdout-based TUI with mouse capture (used by session picker/timeline).
/// Restores terminal + disables mouse capture on drop.
pub struct TerminalGuardMouse;

impl TerminalGuardMouse {
    /// Enter raw mode + alternate screen + mouse capture on stdout.
    pub fn new() -> Result<Self, Box<dyn std::error::Error>> {
        crossterm::terminal::enable_raw_mode()?;
        crossterm::execute!(
            std::io::stdout(),
            crossterm::terminal::EnterAlternateScreen,
            crossterm::event::EnableMouseCapture
        )?;
        Ok(Self)
    }
}

impl Drop for TerminalGuardMouse {
    fn drop(&mut self) {
        let _ = crossterm::terminal::disable_raw_mode();
        let _ = crossterm::execute!(
            std::io::stdout(),
            crossterm::terminal::LeaveAlternateScreen,
            crossterm::event::DisableMouseCapture
        );
    }
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

// ── Shared formatting utilities ─────────────────────────────

/// Format a count with human-readable suffixes (k, M).
#[allow(clippy::cast_precision_loss)]
pub fn format_count(n: i64) -> String {
    if n >= 1_000_000 {
        format!("{:.1}M", n as f64 / 1_000_000.0)
    } else if n >= 1_000 {
        format!("{:.1}k", n as f64 / 1_000.0)
    } else {
        n.to_string()
    }
}

/// Format a duration in milliseconds as a human-readable string.
#[allow(clippy::cast_precision_loss)]
pub fn format_duration_ms(ms: i64) -> String {
    if ms >= 60_000 {
        format!("{:.1}m", ms as f64 / 60_000.0)
    } else if ms >= 1_000 {
        format!("{:.1}s", ms as f64 / 1_000.0)
    } else {
        format!("{ms}ms")
    }
}

/// Return the user's home directory path.
pub fn dirs_home() -> String {
    BaseDirs::new()
        .map(|d| d.home_dir().to_string_lossy().to_string())
        .unwrap_or_default()
}

/// Shorten a path by replacing the home directory prefix with `~`.
pub fn shorten_path(path: &str, home: &str) -> String {
    if !home.is_empty() {
        if let Some(rest) = path.strip_prefix(home) {
            return format!("~{rest}");
        }
    }
    path.to_string()
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

// ── Command syntax highlighting ──────────────────────────────

/// Syntax-highlight a shell command string for TUI display.
///
/// When `wrap_width > 0`, long commands are soft-wrapped at word boundaries
/// so the selected row can show the full command.
pub fn highlight_command(command: &str, wrap_width: usize) -> ratatui::text::Text<'static> {
    use ratatui::style::{Color, Modifier, Style};
    use ratatui::text::{Line, Span};

    let t = crate::theme::theme();
    let mut lines = Vec::new();
    let mut current_line_spans = Vec::new();
    let mut current_line_width = 0;

    let parts: Vec<&str> = command.split_whitespace().collect();
    for (idx, part) in parts.iter().enumerate() {
        let (color, modifier) = if idx == 0 {
            (t.primary, Modifier::BOLD)
        } else if part.starts_with('-') {
            (t.warning, Modifier::empty())
        } else if (part.starts_with('"') && part.ends_with('"'))
            || (part.starts_with('\'') && part.ends_with('\''))
        {
            (Color::Cyan, Modifier::empty())
        } else if part.starts_with('$') {
            (Color::Magenta, Modifier::empty())
        } else if part.contains('/') || part.starts_with('.') || part.starts_with('~') {
            (t.text_secondary, Modifier::empty())
        } else if *part == "|"
            || *part == "&&"
            || *part == "||"
            || *part == ";"
            || *part == ">"
            || *part == ">>"
            || *part == "<"
        {
            (t.info, Modifier::BOLD)
        } else {
            (t.text, Modifier::empty())
        };

        let style = Style::default().fg(color).add_modifier(modifier);
        let part_len = part.chars().count();

        if wrap_width > 0
            && current_line_width + part_len + 1 > wrap_width
            && !current_line_spans.is_empty()
        {
            lines.push(Line::from(current_line_spans.clone()));
            current_line_spans.clear();
            current_line_width = 0;
        }

        current_line_spans.push(Span::styled(part.to_string(), style));
        current_line_spans.push(Span::raw(" "));
        current_line_width += part_len + 1;
    }

    if !current_line_spans.is_empty() {
        lines.push(Line::from(current_line_spans));
    }

    if lines.is_empty() {
        return ratatui::text::Text::from(command.to_string());
    }

    ratatui::text::Text::from(lines)
}

// ── Shell RC cleanup ────────────────────────────────────────

/// Remove the Suvadu initialization line from a shell RC file.
fn cleanup_shell_rc(filename: &str, shell: &str) -> Result<(), std::io::Error> {
    let home = std::env::var("HOME").map_err(|_| {
        std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "HOME environment variable not set",
        )
    })?;
    let rc_path = std::path::PathBuf::from(home).join(filename);

    if !rc_path.exists() {
        return Ok(());
    }

    let content = std::fs::read_to_string(&rc_path)?;
    let target_line = format!("eval \"$(suv init {shell})\"");

    if !content.contains(&target_line) {
        return Ok(());
    }

    let filtered_content: Vec<String> = content
        .lines()
        .filter(|line| line.trim() != target_line)
        .map(String::from)
        .collect();

    let new_content = filtered_content.join("\n") + "\n";
    std::fs::write(&rc_path, new_content)?;

    Ok(())
}

/// Remove the Suvadu initialization line from .zshrc if it exists.
pub fn cleanup_zshrc() -> Result<(), std::io::Error> {
    cleanup_shell_rc(".zshrc", "zsh")
}

/// Remove the Suvadu initialization line from .bashrc if it exists.
pub fn cleanup_bashrc() -> Result<(), std::io::Error> {
    cleanup_shell_rc(".bashrc", "bash")
}

/// Remove the Suvadu hook entry from ~/.claude/settings.json.
/// Returns Ok(true) if a hook was removed, Ok(false) if none found or file doesn't exist.
pub fn cleanup_claude_settings() -> Result<bool, Box<dyn std::error::Error>> {
    let home = std::env::var("HOME")?;
    let settings_path = std::path::PathBuf::from(&home)
        .join(".claude")
        .join("settings.json");

    cleanup_claude_settings_at(&settings_path)
}

/// Remove the Suvadu hook from a specific Claude settings file path.
pub fn cleanup_claude_settings_at(
    settings_path: &std::path::Path,
) -> Result<bool, Box<dyn std::error::Error>> {
    if !settings_path.exists() {
        return Ok(false);
    }

    let content = std::fs::read_to_string(settings_path)?;
    let mut settings: serde_json::Value = serde_json::from_str(&content)?;

    let mut removed = false;

    // Remove suvadu entries from both PostToolUse and UserPromptSubmit
    for key in ["PostToolUse", "UserPromptSubmit"] {
        let Some(arr) = settings
            .get_mut("hooks")
            .and_then(|h| h.get_mut(key))
            .and_then(serde_json::Value::as_array_mut)
        else {
            continue;
        };

        let original_len = arr.len();

        arr.retain(|group| {
            group
                .get("hooks")
                .and_then(serde_json::Value::as_array)
                .is_none_or(|hooks| {
                    !hooks.iter().any(|h| {
                        h.get("command")
                            .and_then(serde_json::Value::as_str)
                            .is_some_and(|cmd| cmd.contains("suvadu"))
                    })
                })
        });

        if arr.len() != original_len {
            removed = true;
        }
    }

    if !removed {
        return Ok(false);
    }

    // Clean up empty containers
    if let Some(hooks) = settings.get_mut("hooks").and_then(|h| h.as_object_mut()) {
        hooks.retain(|_, v| !v.as_array().is_some_and(std::vec::Vec::is_empty));

        if hooks.is_empty() {
            if let Some(root) = settings.as_object_mut() {
                root.remove("hooks");
            }
        }
    }

    let updated = serde_json::to_string_pretty(&settings)?;
    std::fs::write(settings_path, updated)?;

    Ok(true)
}

/// Truncate a string to `max_chars` characters, appending `suffix` if truncated.
/// Safe for multi-byte UTF-8 strings — operates on char boundaries.
pub fn truncate_str(s: &str, max_chars: usize, suffix: &str) -> String {
    let char_count = s.chars().count();
    if char_count <= max_chars {
        return s.to_string();
    }
    let suffix_len = suffix.chars().count();
    if max_chars <= suffix_len {
        return s.chars().take(max_chars).collect();
    }
    let take = max_chars - suffix_len;
    let mut truncated: String = s.chars().take(take).collect();
    truncated.push_str(suffix);
    truncated
}

/// Truncate from the start, keeping the end of the string.
/// Prepends `prefix` if truncated. Safe for multi-byte UTF-8.
pub fn truncate_str_start(s: &str, max_chars: usize, prefix: &str) -> String {
    let char_count = s.chars().count();
    if char_count <= max_chars {
        return s.to_string();
    }
    let prefix_len = prefix.chars().count();
    if max_chars <= prefix_len {
        return s
            .chars()
            .rev()
            .take(max_chars)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect();
    }
    let keep = max_chars - prefix_len;
    let mut result = String::from(prefix);
    result.extend(s.chars().skip(char_count - keep));
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_date_iso() {
        let ts = parse_date_input("2023-01-01", false).unwrap();
        let dt = Local.timestamp_millis_opt(ts).unwrap();
        assert_eq!(
            dt.format("%Y-%m-%d %H:%M:%S").to_string(),
            "2023-01-01 00:00:00"
        );
    }

    #[test]
    fn test_parse_keywords() {
        assert!(parse_date_input("today", false).is_some());
        assert!(parse_date_input("yesterday", true).is_some());
    }

    #[test]
    fn test_is_excluded() {
        let exclusions = vec![
            "^ls$".to_string(),                 // Strict regex
            "password".to_string(),             // Substring (also valid regex)
            "*.log".to_string(), // Invalid regex (glob-like), falls back to substring
            "^git (commit|status)".to_string(), // Complex regex
        ];

        // Strict Regex Match
        assert!(is_excluded("ls", &exclusions));
        assert!(!is_excluded("ls -la", &exclusions)); // Regex ^ls$ doesn't match start/end

        // Substring Match (valid regex "password")
        assert!(is_excluded("echo password123", &exclusions));
        assert!(!is_excluded("echo pass", &exclusions));

        // Substring Fallback (invalid regex "*.log")
        assert!(!is_excluded("tail -f app.log", &exclusions)); // "log" is substring? verify "*.log"
                                                               // Wait, "*.log" IS invalid regex because * cannot start.
                                                               // So it falls back to substring check: command.contains("*.log").
                                                               // "tail -f app.log" does NOT contain "*.log".
                                                               // So this should be FALSE unless command has literal "*.log".
        assert!(!is_excluded("tail -f app.log", &exclusions));
        assert!(is_excluded("rm *.log", &exclusions)); // This contains literally "*.log"

        // Complex Regex
        assert!(is_excluded("git commit -m 'fix'", &exclusions));
        assert!(is_excluded("git status", &exclusions));
        assert!(!is_excluded("git add .", &exclusions));

        // Edge Cases
        let empty: Vec<String> = vec![];
        assert!(!is_excluded("ls", &empty));

        let bad_regex = vec!["[".to_string()]; // Invalid regex
        assert!(is_excluded("this has [ inside", &bad_regex)); // Literal match
        assert!(!is_excluded("normal string", &bad_regex));
    }

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

    #[test]
    fn test_cleanup_claude_settings_removes_suvadu_hook() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("settings.json");

        let settings = serde_json::json!({
            "hooks": {
                "PostToolUse": [{
                    "matcher": "Bash",
                    "hooks": [{
                        "type": "command",
                        "command": "/home/user/.config/suvadu/hooks/claude-code-post-tool.sh"
                    }]
                }]
            }
        });
        std::fs::write(&path, serde_json::to_string_pretty(&settings).unwrap()).unwrap();

        let result = cleanup_claude_settings_at(&path).unwrap();
        assert!(result);

        // Verify the hook was removed and empty containers cleaned up
        let content = std::fs::read_to_string(&path).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&content).unwrap();
        assert!(parsed.get("hooks").is_none());
    }

    #[test]
    fn test_cleanup_claude_settings_preserves_other_hooks() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("settings.json");

        let settings = serde_json::json!({
            "hooks": {
                "PostToolUse": [
                    {
                        "matcher": "Bash",
                        "hooks": [{
                            "type": "command",
                            "command": "/home/user/.config/suvadu/hooks/claude-code-post-tool.sh"
                        }]
                    },
                    {
                        "matcher": "Bash",
                        "hooks": [{
                            "type": "command",
                            "command": "/usr/local/bin/other-hook.sh"
                        }]
                    }
                ]
            }
        });
        std::fs::write(&path, serde_json::to_string_pretty(&settings).unwrap()).unwrap();

        let result = cleanup_claude_settings_at(&path).unwrap();
        assert!(result);

        // Verify only Suvadu hook was removed, other hook preserved
        let content = std::fs::read_to_string(&path).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&content).unwrap();
        let post_tool_use = parsed["hooks"]["PostToolUse"].as_array().unwrap();
        assert_eq!(post_tool_use.len(), 1);
        assert!(post_tool_use[0]["hooks"][0]["command"]
            .as_str()
            .unwrap()
            .contains("other-hook"));
    }

    #[test]
    fn test_cleanup_claude_settings_removes_both_hook_types() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("settings.json");

        let settings = serde_json::json!({
            "hooks": {
                "PostToolUse": [{
                    "matcher": "Bash",
                    "hooks": [{
                        "type": "command",
                        "command": "/home/user/.config/suvadu/hooks/claude-code-post-tool.sh"
                    }]
                }],
                "UserPromptSubmit": [{
                    "hooks": [{
                        "type": "command",
                        "command": "/home/user/.config/suvadu/hooks/claude-code-prompt.sh"
                    }]
                }]
            }
        });
        std::fs::write(&path, serde_json::to_string_pretty(&settings).unwrap()).unwrap();

        let result = cleanup_claude_settings_at(&path).unwrap();
        assert!(result);

        let content = std::fs::read_to_string(&path).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&content).unwrap();
        // Both hook types removed, hooks object cleaned up
        assert!(parsed.get("hooks").is_none());
    }

    #[test]
    fn test_cleanup_claude_settings_no_file() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("nonexistent.json");

        let result = cleanup_claude_settings_at(&path).unwrap();
        assert!(!result);
    }

    #[test]
    fn test_format_count() {
        // Below 1000: plain number
        assert_eq!(format_count(0), "0");
        assert_eq!(format_count(500), "500");
        assert_eq!(format_count(999), "999");

        // 1000+: k suffix
        assert_eq!(format_count(1000), "1.0k");
        assert_eq!(format_count(999_999), "1000.0k");

        // 1_000_000+: M suffix
        assert_eq!(format_count(1_000_000), "1.0M");
    }

    #[test]
    fn test_format_duration_ms() {
        // Under 1s: milliseconds
        assert_eq!(format_duration_ms(0), "0ms");
        assert_eq!(format_duration_ms(500), "500ms");

        // 1s+: seconds
        assert_eq!(format_duration_ms(1000), "1.0s");
        assert_eq!(format_duration_ms(59_999), "60.0s");

        // 60s+: minutes
        assert_eq!(format_duration_ms(60_000), "1.0m");
        assert_eq!(format_duration_ms(120_000), "2.0m");
    }

    #[test]
    fn test_shorten_path() {
        let home = "/Users/testuser";

        // Path under home -> replaced with ~
        assert_eq!(shorten_path("/Users/testuser/projects", home), "~/projects");

        // Path NOT under home -> unchanged
        assert_eq!(shorten_path("/var/log/syslog", home), "/var/log/syslog");

        // Empty home -> path unchanged
        assert_eq!(
            shorten_path("/Users/testuser/projects", ""),
            "/Users/testuser/projects"
        );

        // Exact home path -> just ~
        assert_eq!(shorten_path("/Users/testuser", home), "~");
    }

    #[test]
    fn test_dirs_home() {
        let home = dirs_home();
        // Should return a non-empty string on any real system
        assert!(
            !home.is_empty(),
            "dirs_home() should return a non-empty path"
        );
        // On macOS/Linux, should start with /
        assert!(
            home.starts_with('/'),
            "Home directory should be an absolute path, got: {home}"
        );
    }

    #[test]
    fn test_truncate_str_short() {
        assert_eq!(truncate_str("hello", 10, "…"), "hello");
    }

    #[test]
    fn test_truncate_str_exact() {
        assert_eq!(truncate_str("hello", 5, "…"), "hello");
    }

    #[test]
    fn test_truncate_str_truncated() {
        assert_eq!(truncate_str("hello world", 8, "…"), "hello w…");
    }

    #[test]
    fn test_truncate_str_unicode() {
        // Japanese characters are multi-byte UTF-8
        let s = "こんにちは世界テスト";
        assert_eq!(truncate_str(s, 6, "…"), "こんにちは…");
    }

    #[test]
    fn test_truncate_str_emoji() {
        let s = "hello 🌍🌎🌏 world";
        let result = truncate_str(s, 10, "…");
        assert_eq!(result.chars().count(), 10);
        assert!(result.ends_with('…'));
    }

    #[test]
    fn test_truncate_str_tiny_max() {
        assert_eq!(truncate_str("hello world", 1, "…"), "h");
        assert_eq!(truncate_str("hello world", 0, "…"), "");
    }

    #[test]
    fn test_truncate_str_start_short() {
        assert_eq!(truncate_str_start("hello", 10, "…"), "hello");
    }

    #[test]
    fn test_truncate_str_start_truncated() {
        assert_eq!(
            truncate_str_start("/very/long/path/to/dir", 15, "…"),
            "…ng/path/to/dir"
        );
    }

    #[test]
    fn test_truncate_str_start_unicode() {
        let s = "あいうえおかきくけこ";
        let result = truncate_str_start(s, 6, "…");
        assert_eq!(result.chars().count(), 6);
        assert!(result.starts_with('…'));
        assert!(result.ends_with('こ'));
    }
}
