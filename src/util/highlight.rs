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
