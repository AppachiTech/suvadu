use ratatui::style::Style;
use ratatui::text::{Line, Span};
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

/// Break one styled `line` into rows at most `width` cells wide, keeping
/// each span's style. With `at_spaces`, a row ends after its last space when
/// the next word does not fit (a word wider than a row is still broken
/// between graphemes); without it, rows break wherever they are full.
///
/// The caller draws the rows unwrapped, so the row count is exact — what a
/// scrolled view needs to reach its last row. Control characters are
/// dropped, as ratatui drops them when drawing.
pub fn wrap_line(line: &Line<'_>, width: usize, at_spaces: bool) -> Vec<Line<'static>> {
    let width = width.max(1);
    let mut rows: Vec<Vec<(&str, Style, usize)>> = Vec::new();
    let mut row: Vec<(&str, Style, usize)> = Vec::new();
    let mut used = 0;
    // Index in `row` of its last space, where a word-preferring break goes.
    let mut last_space: Option<usize> = None;
    for span in &line.spans {
        for g in span.content.graphemes(true) {
            if g.contains(char::is_control) {
                continue;
            }
            let w = g.width();
            if used > 0 && used + w > width {
                if let Some(at) = last_space.take().filter(|_| at_spaces) {
                    // The word being built moves down whole; the space ends
                    // the row above.
                    let word = row.split_off(at + 1);
                    rows.push(std::mem::replace(&mut row, word));
                    used = row.iter().map(|&(_, _, w)| w).sum();
                    if used > 0 && used + w > width {
                        rows.push(std::mem::take(&mut row));
                        used = 0;
                    }
                } else {
                    rows.push(std::mem::take(&mut row));
                    used = 0;
                }
            }
            row.push((g, span.style, w));
            used += w;
            if g == " " {
                last_space = Some(row.len() - 1);
            }
        }
    }
    rows.push(row);
    rows.into_iter()
        .map(|cells| to_line(&cells).style(line.style))
        .collect()
}

/// Rejoin a row's graphemes into spans, one per run of the same style.
fn to_line(cells: &[(&str, Style, usize)]) -> Line<'static> {
    let mut spans: Vec<Span<'static>> = Vec::new();
    for &(g, style, _) in cells {
        match spans.last_mut() {
            Some(last) if last.style == style => last.content.to_mut().push_str(g),
            _ => spans.push(Span::styled(g.to_string(), style)),
        }
    }
    Line::from(spans)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::style::Color;

    fn text(rows: &[Line<'_>]) -> Vec<String> {
        rows.iter().map(ToString::to_string).collect()
    }

    #[test]
    fn breaks_between_graphemes_keeping_each_style() {
        let red = Style::default().fg(Color::Red);
        let line = Line::from(vec![Span::raw("ab"), Span::styled("cdef", red)]);
        let rows = wrap_line(&line, 3, false);
        assert_eq!(text(&rows), ["abc", "def"]);
        assert_eq!(rows[0].spans[1].style, red);
        assert_eq!(rows[1].spans[0].style, red);
    }

    #[test]
    fn at_spaces_moves_a_word_down_whole() {
        let rows = wrap_line(&Line::raw("cargo build --release"), 10, true);
        assert_eq!(text(&rows), ["cargo ", "build ", "--release"]);
        // The same text broken wherever a row is full.
        let rows = wrap_line(&Line::raw("cargo build --release"), 10, false);
        assert_eq!(text(&rows), ["cargo buil", "d --releas", "e"]);
        let rows = wrap_line(&Line::raw("/home/user/project dir"), 10, true);
        assert_eq!(text(&rows), ["/home/user", "/project ", "dir"]);
    }

    #[test]
    fn a_word_wider_than_a_row_is_still_broken() {
        let rows = wrap_line(&Line::raw("a abcdefgh"), 4, true);
        assert_eq!(text(&rows), ["a ", "abcd", "efgh"]);
    }

    #[test]
    fn wide_and_control_characters_never_overflow_a_row() {
        let rows = wrap_line(&Line::raw("日本語"), 4, false);
        assert_eq!(text(&rows), ["日本", "語"]);
        // Control characters take no cell, as when ratatui draws them.
        let rows = wrap_line(&Line::from(Span::raw("x\ny")), 3, false);
        assert_eq!(text(&rows), ["xy"]);
        assert_eq!(text(&wrap_line(&Line::default(), 3, true)), [""]);
    }
}
