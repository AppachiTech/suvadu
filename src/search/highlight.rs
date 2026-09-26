//! How a command is drawn in the results list.
//!
//! Two jobs sit on top of the syntax colouring every `suv` screen uses:
//!
//! * **Why it matched.** The characters the query matched are emphasised,
//!   and the emphasis wins over syntax colour, so a path-heavy command that
//!   matched `suv u` shows where. `terms` marks every occurrence of each
//!   word, `literal` the whole query, `prefix` the start, and `fuzzy` the
//!   characters its subsequence rule actually walked.
//! * **What is really stored.** Grouping is by exact command text, so
//!   `suv status` and `suv status  ` are two commands. Leading and trailing
//!   whitespace is drawn as visible markers and runs of spaces are kept, so
//!   two rows never look identical while being different commands. The
//!   stored text is never changed.

use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span, Text};

use super::MatchMode;

/// For each character of `command`, whether the query matched it under
/// `mode`. Case is folded per character, which is what matching does for
/// the characters people type; the database, not this, decides eligibility.
pub(super) fn match_mask(command: &str, query: &str, mode: MatchMode) -> Vec<bool> {
    let hay: Vec<char> = command.chars().map(fold).collect();
    let mut mask = vec![false; hay.len()];
    let query = query.trim();
    if query.is_empty() {
        return mask;
    }
    let mut mark_all = |needle: &str| {
        let needle: Vec<char> = needle.chars().map(fold).collect();
        if needle.is_empty() || needle.len() > hay.len() {
            return;
        }
        for start in 0..=hay.len() - needle.len() {
            if hay[start..start + needle.len()] == needle[..] {
                mask[start..start + needle.len()].fill(true);
            }
        }
    };
    match mode {
        MatchMode::Terms => {
            for term in query.split_whitespace() {
                mark_all(term);
            }
        }
        MatchMode::Literal => mark_all(query),
        MatchMode::Prefix => {
            let needle: Vec<char> = query.chars().map(fold).collect();
            if hay.starts_with(&needle) {
                mask[..needle.len()].fill(true);
            }
        }
        MatchMode::Fuzzy => {
            // The same greedy walk `is_subsequence_ci` makes: each query
            // character takes the first unused match after the previous one.
            let mut from = 0;
            for c in query.chars().map(fold) {
                match hay[from..].iter().position(|&h| h == c) {
                    Some(offset) => {
                        mask[from + offset] = true;
                        from += offset + 1;
                    }
                    None => return vec![false; hay.len()],
                }
            }
        }
    }
    mask
}

fn fold(c: char) -> char {
    c.to_lowercase().next().unwrap_or(c)
}

/// Syntax colours for a command, emphasis for matched characters, and
/// whitespace drawn as stored.
pub(super) struct CommandStyle {
    pub text: Color,
    pub program: Color,
    pub flag: Color,
    pub quoted: Color,
    pub variable: Color,
    pub path: Color,
    pub operator: Color,
    pub marker: Color,
    pub matched: Style,
}

impl CommandStyle {
    pub(super) fn from_theme(t: &crate::theme::Theme) -> Self {
        Self {
            text: t.text,
            program: t.primary,
            flag: t.warning,
            quoted: Color::Cyan,
            variable: Color::Magenta,
            path: t.text_secondary,
            operator: t.info,
            marker: t.text_muted,
            // Colour plus bold and underline, so the emphasis survives a
            // monochrome terminal and the selected row's background.
            matched: Style::default()
                .fg(t.warning)
                .add_modifier(Modifier::BOLD | Modifier::UNDERLINED),
        }
    }

    /// The same word classes as `util::highlight_command`.
    fn word(&self, index: usize, word: &str) -> Style {
        let (fg, modifier) = if index == 0 {
            (self.program, Modifier::BOLD)
        } else if word.starts_with('-') {
            (self.flag, Modifier::empty())
        } else if (word.starts_with('"') && word.ends_with('"'))
            || (word.starts_with('\'') && word.ends_with('\''))
        {
            (self.quoted, Modifier::empty())
        } else if word.starts_with('$') {
            (self.variable, Modifier::empty())
        } else if word.contains('/') || word.starts_with('.') || word.starts_with('~') {
            (self.path, Modifier::empty())
        } else if matches!(word, "|" | "&&" | "||" | ";" | ">" | ">>" | "<") {
            (self.operator, Modifier::BOLD)
        } else {
            (self.text, Modifier::empty())
        };
        Style::default().fg(fg).add_modifier(modifier)
    }
}

/// Most whitespace markers drawn at either end before the rest is summed up.
const MAX_EDGE_MARKERS: usize = 8;

/// One unit the line wrapper places: a word (never split across lines) or
/// the whitespace between words.
struct Piece {
    spans: Vec<Span<'static>>,
    width: usize,
    is_space: bool,
}

/// Draw `command` after `prefix` (bookmark/note/count markers), emphasising
/// the characters `mask` marks. With `wrap_width > 0` the command wraps at
/// word boundaries, as the selected row does.
pub(super) fn command_text(
    prefix: Vec<Span<'static>>,
    command: &str,
    wrap_width: usize,
    mask: &[bool],
    style: &CommandStyle,
) -> Text<'static> {
    let chars: Vec<char> = command.chars().collect();
    let matched = |i: usize| mask.get(i).copied().unwrap_or(false);
    let lead = chars.iter().take_while(|c| c.is_whitespace()).count();
    let trail = if lead == chars.len() {
        0
    } else {
        chars.iter().rev().take_while(|c| c.is_whitespace()).count()
    };

    let mut pieces: Vec<Piece> = Vec::new();
    if !prefix.is_empty() {
        let width = prefix
            .iter()
            .map(|s| super::format::display_width(&s.content))
            .sum();
        pieces.push(Piece {
            spans: prefix,
            width,
            is_space: false,
        });
    }
    if lead > 0 {
        pieces.push(edge_markers(&chars[..lead], style));
    }

    let body_end = chars.len() - trail;
    let mut i = lead;
    let mut word_index = 0;
    while i < body_end {
        let start = i;
        if chars[i].is_whitespace() {
            while i < body_end && chars[i].is_whitespace() {
                i += 1;
            }
            pieces.push(inner_space(&chars[start..i], start, &matched, style));
        } else {
            while i < body_end && !chars[i].is_whitespace() {
                i += 1;
            }
            let word: String = chars[start..i].iter().collect();
            let base = style.word(word_index, &word);
            word_index += 1;
            let mut spans = Vec::new();
            let mut run = String::new();
            let mut run_matched = matched(start);
            for (offset, c) in word.chars().enumerate() {
                let m = matched(start + offset);
                if m != run_matched && !run.is_empty() {
                    spans.push(Span::styled(
                        std::mem::take(&mut run),
                        if run_matched { style.matched } else { base },
                    ));
                }
                run_matched = m;
                run.push(c);
            }
            if !run.is_empty() {
                spans.push(Span::styled(
                    run,
                    if run_matched { style.matched } else { base },
                ));
            }
            pieces.push(Piece {
                spans,
                width: super::format::display_width(&word),
                is_space: false,
            });
        }
    }
    if trail > 0 {
        pieces.push(edge_markers(&chars[body_end..], style));
    }

    wrap(pieces, wrap_width)
}

/// Whitespace between words: spaces as stored, tabs and line breaks as
/// visible markers, since a list row is a single line.
fn inner_space(
    run: &[char],
    start: usize,
    matched: &impl Fn(usize) -> bool,
    style: &CommandStyle,
) -> Piece {
    if run.iter().all(|&c| c == ' ') {
        let text = " ".repeat(run.len());
        let styled = if (start..start + run.len()).any(matched) {
            style.matched
        } else {
            Style::default()
        };
        return Piece {
            width: run.len(),
            spans: vec![Span::styled(text, styled)],
            is_space: true,
        };
    }
    let marker = if run.contains(&'\n') {
        "\u{21b5}"
    } else {
        "\u{21e5}"
    };
    Piece {
        spans: vec![
            Span::raw(" "),
            Span::styled(marker, Style::default().fg(style.marker)),
            Span::raw(" "),
        ],
        width: 3,
        is_space: true,
    }
}

/// Leading or trailing whitespace, one marker per character.
fn edge_markers(run: &[char], style: &CommandStyle) -> Piece {
    let mut text: String = run
        .iter()
        .take(MAX_EDGE_MARKERS)
        .map(|&c| match c {
            '\n' => '\u{21b5}',
            '\t' => '\u{21e5}',
            _ => '\u{b7}',
        })
        .collect();
    if run.len() > MAX_EDGE_MARKERS {
        text.push('\u{2026}');
    }
    let width = super::format::display_width(&text);
    Piece {
        spans: vec![Span::styled(text, Style::default().fg(style.marker))],
        width,
        is_space: false,
    }
}

/// Lay pieces out on lines no wider than `wrap_width` (0: one line), moving
/// a word that would overflow to the next line and dropping the whitespace
/// the break replaces.
fn wrap(pieces: Vec<Piece>, wrap_width: usize) -> Text<'static> {
    let mut lines: Vec<Line<'static>> = Vec::new();
    let mut spans: Vec<Span<'static>> = Vec::new();
    let mut width = 0;
    for piece in pieces {
        if wrap_width > 0 && !piece.is_space && width > 0 && width + piece.width > wrap_width {
            // Trailing whitespace on the line being closed is the break.
            while spans.last().is_some_and(|s| s.content.trim().is_empty()) {
                spans.pop();
            }
            lines.push(Line::from(std::mem::take(&mut spans)));
            width = 0;
        }
        if piece.is_space && width == 0 && !lines.is_empty() {
            continue;
        }
        width += piece.width;
        spans.extend(piece.spans);
    }
    if !spans.is_empty() || lines.is_empty() {
        lines.push(Line::from(spans));
    }
    Text::from(lines)
}

/// `"just now"`, `"5m ago"`, `"3h ago"`, `"yesterday"`, `"4d ago"`,
/// `"3w ago"`, `"7mo ago"`, `"2y ago"` — short enough for a list column.
pub(super) fn relative_age(now_ms: i64, then_ms: i64) -> String {
    const MIN: i64 = 60_000;
    const HOUR: i64 = 60 * MIN;
    const DAY: i64 = 24 * HOUR;
    let age = now_ms.saturating_sub(then_ms).max(0);
    match age {
        a if a < MIN => "just now".to_string(),
        a if a < HOUR => format!("{}m ago", a / MIN),
        a if a < DAY => format!("{}h ago", a / HOUR),
        a if a < 2 * DAY => "yesterday".to_string(),
        a if a < 14 * DAY => format!("{}d ago", a / DAY),
        a if a < 60 * DAY => format!("{}w ago", a / (7 * DAY)),
        a if a < 365 * DAY => format!("{}mo ago", a / (30 * DAY)),
        a => format!("{}y ago", a / (365 * DAY)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn marked(command: &str, query: &str, mode: MatchMode) -> String {
        command
            .chars()
            .zip(match_mask(command, query, mode))
            .map(|(c, m)| if m { c.to_ascii_uppercase() } else { '.' })
            .collect()
    }

    #[test]
    fn terms_mark_every_occurrence_of_each_word() {
        assert_eq!(
            marked("suv update && suvadu update", "suv u", MatchMode::Terms),
            "SUV.U.........SUV..U.U....."
        );
    }

    #[test]
    fn literal_prefix_and_fuzzy_mark_what_their_rule_matched() {
        assert_eq!(
            marked("git commit -m x", "git c", MatchMode::Literal),
            // The space is part of a literal query, and is marked too.
            "GIT C.........."
        );
        assert_eq!(marked("cargo test", "car", MatchMode::Prefix), "CAR.......");
        assert_eq!(marked("test cargo", "car", MatchMode::Prefix), "..........");
        assert_eq!(
            marked("git checkout main", "gco", MatchMode::Fuzzy),
            "G...C....O......."
        );
    }

    #[test]
    fn matching_folds_case_including_non_ascii() {
        assert_eq!(marked("echo Écho", "écho", MatchMode::Terms), ".....ÉCHO");
    }

    #[test]
    fn an_empty_query_marks_nothing() {
        assert!(match_mask("ls -la", "  ", MatchMode::Terms)
            .iter()
            .all(|m| !m));
    }

    fn plain(text: &Text<'_>) -> Vec<String> {
        text.lines
            .iter()
            .map(|l| l.spans.iter().map(|s| s.content.as_ref()).collect())
            .collect()
    }

    fn style() -> CommandStyle {
        CommandStyle::from_theme(crate::theme::theme())
    }

    #[test]
    fn trailing_whitespace_is_visible_so_distinct_commands_never_look_alike() {
        let a = command_text(vec![], "suv status", 0, &[], &style());
        let b = command_text(vec![], "suv status  ", 0, &[], &style());
        assert_eq!(plain(&a), vec!["suv status"]);
        assert_eq!(plain(&b), vec!["suv status\u{b7}\u{b7}"]);
        let c = command_text(vec![], " suv  status", 0, &[], &style());
        assert_eq!(plain(&c), vec!["\u{b7}suv  status"]);
    }

    #[test]
    fn line_breaks_and_tabs_inside_a_command_are_marked() {
        let text = command_text(
            vec![],
            "for i in 1 2; do\n  echo $i\ndone",
            0,
            &[],
            &style(),
        );
        assert_eq!(
            plain(&text),
            vec!["for i in 1 2; do \u{21b5} echo $i \u{21b5} done"]
        );
    }

    #[test]
    fn matched_characters_take_the_emphasis_style() {
        let s = style();
        let mask = match_mask("git checkout", "check", MatchMode::Terms);
        let text = command_text(vec![], "git checkout", 0, &mask, &s);
        let emphasised: String = text.lines[0]
            .spans
            .iter()
            .filter(|span| span.style == s.matched)
            .map(|span| span.content.as_ref())
            .collect();
        assert_eq!(emphasised, "check");
    }

    #[test]
    fn wrapping_breaks_between_words_and_keeps_the_prefix() {
        let prefix = vec![Span::raw("26\u{d7} ")];
        let text = command_text(prefix, "docker compose up --build web", 16, &[], &style());
        assert_eq!(
            plain(&text),
            vec!["26\u{d7} docker", "compose up", "--build web"]
        );
    }

    #[test]
    fn relative_ages_are_short_and_monotonic() {
        let now = 10_000_000_000;
        let min = 60_000;
        let hour = 60 * min;
        let day = 24 * hour;
        let cases = [
            (0, "just now"),
            (5 * min, "5m ago"),
            (3 * hour, "3h ago"),
            (30 * hour, "yesterday"),
            (4 * day, "4d ago"),
            (21 * day, "3w ago"),
            (200 * day, "6mo ago"),
            (800 * day, "2y ago"),
        ];
        for (age, expected) in cases {
            assert_eq!(relative_age(now, now - age), expected, "{age}");
        }
        // A clock that ran backwards is not "in the future".
        assert_eq!(relative_age(now, now + day), "just now");
    }
}
