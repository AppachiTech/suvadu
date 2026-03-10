use directories::BaseDirs;

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

/// Truncate a string to `max_cols` display columns, appending `suffix` if truncated.
/// Uses unicode display width so CJK and other wide characters are measured correctly.
pub fn truncate_str(s: &str, max_cols: usize, suffix: &str) -> String {
    use unicode_width::UnicodeWidthChar;

    let width: usize = s.chars().filter_map(UnicodeWidthChar::width).sum();
    if width <= max_cols {
        return s.to_string();
    }
    let suffix_width: usize = suffix.chars().filter_map(UnicodeWidthChar::width).sum();
    if max_cols <= suffix_width {
        // Not enough room for suffix — just take what fits
        let mut result = String::new();
        let mut used = 0;
        for c in s.chars() {
            let w = UnicodeWidthChar::width(c).unwrap_or(0);
            if used + w > max_cols {
                break;
            }
            result.push(c);
            used += w;
        }
        return result;
    }
    let budget = max_cols - suffix_width;
    let mut result = String::new();
    let mut used = 0;
    for c in s.chars() {
        let w = UnicodeWidthChar::width(c).unwrap_or(0);
        if used + w > budget {
            break;
        }
        result.push(c);
        used += w;
    }
    result.push_str(suffix);
    result
}

/// Truncate from the start, keeping the end of the string.
/// Prepends `prefix` if truncated. Uses unicode display width.
pub fn truncate_str_start(s: &str, max_cols: usize, prefix: &str) -> String {
    use unicode_width::UnicodeWidthChar;

    let width: usize = s.chars().filter_map(UnicodeWidthChar::width).sum();
    if width <= max_cols {
        return s.to_string();
    }
    let prefix_width: usize = prefix.chars().filter_map(UnicodeWidthChar::width).sum();
    if max_cols <= prefix_width {
        // Not enough room for prefix — take from the end
        let mut chars: Vec<char> = Vec::new();
        let mut used = 0;
        for c in s.chars().rev() {
            let w = UnicodeWidthChar::width(c).unwrap_or(0);
            if used + w > max_cols {
                break;
            }
            chars.push(c);
            used += w;
        }
        chars.reverse();
        return chars.into_iter().collect();
    }
    let budget = max_cols - prefix_width;
    // Collect chars with widths from the end
    let chars_vec: Vec<(char, usize)> = s
        .chars()
        .map(|c| (c, UnicodeWidthChar::width(c).unwrap_or(0)))
        .collect();
    let mut used = 0;
    let mut start_idx = chars_vec.len();
    for i in (0..chars_vec.len()).rev() {
        if used + chars_vec[i].1 > budget {
            break;
        }
        used += chars_vec[i].1;
        start_idx = i;
    }
    let mut result = String::from(prefix);
    for &(c, _) in &chars_vec[start_idx..] {
        result.push(c);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

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
        // Japanese characters are 2 display columns each, "…" is 1 column.
        // Budget 7 cols: suffix "…" = 1 col, so 6 cols for content = 3 CJK chars.
        let s = "こんにちは世界テスト";
        assert_eq!(truncate_str(s, 7, "…"), "こんに…");
    }

    #[test]
    fn test_truncate_str_emoji() {
        // 🌍🌎🌏 are 2 display columns each.
        // "hello " = 6 cols, then we need to fit within 10 cols total with "…" suffix.
        let s = "hello 🌍🌎🌏 world";
        let result = truncate_str(s, 10, "…");
        assert!(result.ends_with('…'));
        // "hello 🌍…" = 6 + 2 + 1 = 9 cols (fits). Adding 🌎 would be 11.
        assert_eq!(result, "hello 🌍…");
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
        // Each CJK char = 2 cols, "…" = 1 col.
        // Budget 7 cols: prefix = 1 col, 6 cols left = 3 CJK chars from end.
        let s = "あいうえおかきくけこ";
        let result = truncate_str_start(s, 7, "…");
        assert!(result.starts_with('…'));
        assert!(result.ends_with('こ'));
        assert_eq!(result, "…くけこ");
    }
}
