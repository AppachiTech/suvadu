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
/// `mode` — by the same case rule the database used to find the row:
///
/// * `terms` answers an ASCII word with `LIKE`, which folds ASCII case only,
///   and a non-ASCII word with `suvadu_contains_ci`, which compares the full
///   Unicode lowercase of both strings;
/// * `literal` and `prefix` are `LIKE`, so ASCII case only;
/// * `fuzzy` walks the full Unicode lowercase, as `is_subsequence_ci` does.
///
/// Unicode lowercasing can expand a character (`İ` becomes two) and depends
/// on context (a final `Σ` becomes `ς`), so matches are found in the
/// lowercased text and mapped back to the characters that produced them.
pub(super) fn match_mask(command: &str, query: &str, mode: MatchMode) -> Vec<bool> {
    let chars: Vec<char> = command.chars().collect();
    let mut mask = vec![false; chars.len()];
    let query = query.trim();
    if query.is_empty() {
        return mask;
    }
    match mode {
        MatchMode::Terms => {
            for term in query.split_whitespace() {
                if term.is_ascii() {
                    mark_ascii_folded(&chars, term, &mut mask);
                } else {
                    mark_unicode_folded(command, term, &mut mask);
                }
            }
        }
        MatchMode::Literal => mark_ascii_folded(&chars, query, &mut mask),
        MatchMode::Prefix => {
            let needle: Vec<char> = query.chars().collect();
            if chars.len() >= needle.len()
                && chars
                    .iter()
                    .zip(&needle)
                    .all(|(a, b)| a.eq_ignore_ascii_case(b))
            {
                mask[..needle.len()].fill(true);
            }
        }
        MatchMode::Fuzzy => {
            let (lower, owner) = lowercase_with_owners(command);
            let lower: Vec<char> = lower.chars().collect();
            let mut from = 0;
            for c in query.to_lowercase().chars() {
                match lower[from..].iter().position(|&h| h == c) {
                    Some(offset) => {
                        mask[owner[from + offset]] = true;
                        from += offset + 1;
                    }
                    None => return vec![false; chars.len()],
                }
            }
        }
    }
    mask
}

/// Mark every occurrence of `needle`, comparing ASCII letters without case
/// and everything else exactly — what `LIKE` does.
fn mark_ascii_folded(chars: &[char], needle: &str, mask: &mut [bool]) {
    let needle: Vec<char> = needle.chars().collect();
    if needle.is_empty() || needle.len() > chars.len() {
        return;
    }
    for start in 0..=chars.len() - needle.len() {
        if chars[start..start + needle.len()]
            .iter()
            .zip(&needle)
            .all(|(a, b)| a.eq_ignore_ascii_case(b))
        {
            mask[start..start + needle.len()].fill(true);
        }
    }
}

/// Mark every occurrence of `needle` in the full Unicode lowercase of
/// `command`, crediting the original characters that produced it.
fn mark_unicode_folded(command: &str, needle: &str, mask: &mut [bool]) {
    let (lower, owner) = lowercase_with_owners(command);
    let needle = needle.to_lowercase();
    if needle.is_empty() {
        return;
    }
    // `owner` is indexed by character; find each match's character span.
    let char_at_byte: Vec<usize> = {
        let mut map = vec![0; lower.len() + 1];
        for (k, (byte, _)) in lower.char_indices().enumerate() {
            map[byte] = k;
        }
        map[lower.len()] = lower.chars().count();
        map
    };
    for (byte, found) in lower.match_indices(&needle) {
        let (first, last) = (char_at_byte[byte], char_at_byte[byte + found.len()]);
        for &original in &owner[first..last] {
            mask[original] = true;
        }
    }
}

/// `command.to_lowercase()`, and for each of its characters the index of
/// the original character it came from. Context changes a character's
/// lowercase (final sigma) but never how many characters it produces, so
/// the per-character expansion lines up with the whole-string result.
fn lowercase_with_owners(command: &str) -> (String, Vec<usize>) {
    let lower = command.to_lowercase();
    let mut owner = Vec::with_capacity(lower.len());
    for (i, c) in command.chars().enumerate() {
        owner.extend(std::iter::repeat_n(i, c.to_lowercase().count()));
    }
    debug_assert_eq!(owner.len(), lower.chars().count());
    owner.resize(
        lower.chars().count(),
        command.chars().count().saturating_sub(1),
    );
    (lower, owner)
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

/// How a command is fitted to its column.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Fit {
    /// One line, as long as it is (the caller clips).
    Unlimited,
    /// Wrap at word boundaries within this many cells, breaking a word that
    /// is wider than a whole line between graphemes — the selected row,
    /// which must show every character.
    Wrap(usize),
    /// One line within this many cells, ending in `…` when anything had to
    /// be cut, so a clipped row never looks complete.
    Truncate(usize),
}

/// One drawn grapheme: its text, its style and how many terminal cells it
/// occupies.
#[derive(Clone)]
struct Glyph {
    text: String,
    style: Style,
    width: usize,
}

impl Glyph {
    fn new(text: &str, style: Style) -> Self {
        Self {
            text: text.to_string(),
            style,
            width: super::format::display_width(text),
        }
    }
}

/// One unit the line wrapper places: a word (kept whole unless it is wider
/// than a line) or the whitespace between words.
struct Piece {
    glyphs: Vec<Glyph>,
    is_space: bool,
}

impl Piece {
    fn width(&self) -> usize {
        self.glyphs.iter().map(|g| g.width).sum()
    }
}

/// Draw `command` after `prefix` (bookmark/note/count markers), emphasising
/// the characters `mask` marks, fitted to its column by `fit`.
///
/// Everything is laid out by grapheme cluster, the unit a terminal draws: a
/// span boundary inside `e` + combining accent, or inside a joined emoji,
/// would split what is one visible character. A grapheme any part of which
/// matched is emphasised whole.
pub(super) fn command_text(
    prefix: &[Span<'static>],
    command: &str,
    fit: Fit,
    mask: &[bool],
    style: &CommandStyle,
) -> Text<'static> {
    use unicode_segmentation::UnicodeSegmentation;

    let mut graphemes: Vec<Grapheme<'_>> = Vec::new();
    let mut char_index = 0;
    for g in command.graphemes(true) {
        let len = g.chars().count();
        graphemes.push(Grapheme {
            text: g,
            matched: (char_index..char_index + len).any(|i| mask.get(i).copied().unwrap_or(false)),
            space: g.chars().all(char::is_whitespace),
        });
        char_index += len;
    }
    let lead = graphemes.iter().take_while(|g| g.space).count();
    let trail = if lead == graphemes.len() {
        0
    } else {
        graphemes.iter().rev().take_while(|g| g.space).count()
    };

    let mut pieces: Vec<Piece> = Vec::new();
    if !prefix.is_empty() {
        let glyphs = prefix
            .iter()
            .flat_map(|span| {
                span.content
                    .graphemes(true)
                    .map(|g| Glyph::new(g, span.style))
                    .collect::<Vec<_>>()
            })
            .collect();
        pieces.push(Piece {
            glyphs,
            is_space: false,
        });
    }
    if lead > 0 {
        pieces.push(edge_markers(&graphemes[..lead], style));
    }

    let body_end = graphemes.len() - trail;
    let mut i = lead;
    let mut word_index = 0;
    while i < body_end {
        let start = i;
        let space = graphemes[i].space;
        while i < body_end && graphemes[i].space == space {
            i += 1;
        }
        let run = &graphemes[start..i];
        if space {
            pieces.push(inner_space(run, style));
            continue;
        }
        let word: String = run.iter().map(|g| g.text).collect();
        let base = style.word(word_index, &word);
        word_index += 1;
        pieces.push(Piece {
            glyphs: run
                .iter()
                .map(|g| Glyph::new(g.text, if g.matched { style.matched } else { base }))
                .collect(),
            is_space: false,
        });
    }
    if trail > 0 {
        pieces.push(edge_markers(&graphemes[body_end..], style));
    }

    let ellipsis = Glyph::new("\u{2026}", Style::default().fg(style.marker));
    let lines = match fit {
        Fit::Wrap(width) if width > 0 => wrap(pieces, width),
        Fit::Truncate(width) if width > 0 => vec![truncate(pieces, width, ellipsis)],
        _ => vec![pieces.into_iter().flat_map(|p| p.glyphs).collect()],
    };
    Text::from(lines.into_iter().map(to_line).collect::<Vec<_>>())
}

/// One grapheme cluster of the command, and what the renderer needs to
/// know about it.
struct Grapheme<'a> {
    text: &'a str,
    matched: bool,
    space: bool,
}

/// The marker drawn for a whitespace character that is not a plain space:
/// every one gets its own, so two tabs never look like one.
fn whitespace_marker(grapheme: &str) -> &'static str {
    if grapheme.contains('\n') {
        "\u{21b5}"
    } else if grapheme == "\t" {
        "\u{21e5}"
    } else {
        // A carriage return, a non-breaking or other Unicode space: not a
        // plain space, so it must not look like one.
        "\u{b7}"
    }
}

/// Whitespace between words: plain spaces as stored, and one visible
/// marker for every other whitespace character, since a list row is a
/// single line.
fn inner_space(run: &[Grapheme<'_>], style: &CommandStyle) -> Piece {
    let marker = Style::default().fg(style.marker);
    let emphasis = if run.iter().any(|g| g.matched) {
        style.matched
    } else {
        Style::default()
    };
    let plain = run.iter().all(|g| g.text == " ");
    Piece {
        glyphs: run
            .iter()
            .map(|g| {
                if g.text == " " {
                    Glyph::new(" ", emphasis)
                } else {
                    Glyph::new(whitespace_marker(g.text), marker)
                }
            })
            .collect(),
        // A run carrying markers is content: a wrap must not swallow it.
        is_space: plain,
    }
}

/// Leading or trailing whitespace, one marker per character. A long run
/// shows its first markers and how many more there are, so runs of
/// different lengths never look the same.
fn edge_markers(run: &[Grapheme<'_>], style: &CommandStyle) -> Piece {
    let marker = Style::default().fg(style.marker);
    let mut glyphs: Vec<Glyph> = run
        .iter()
        .take(MAX_EDGE_MARKERS)
        .map(|g| {
            let symbol = if g.text == " " {
                "\u{b7}"
            } else {
                whitespace_marker(g.text)
            };
            Glyph::new(symbol, marker)
        })
        .collect();
    if run.len() > MAX_EDGE_MARKERS {
        glyphs.push(Glyph::new(
            &format!("+{}", run.len() - MAX_EDGE_MARKERS),
            marker,
        ));
    }
    Piece {
        glyphs,
        is_space: false,
    }
}

/// Characters that draw as nothing at all: zero-width spaces, direction
/// marks and overrides, the byte-order mark, soft hyphens and the like. Two
/// commands differing only by one of these look identical.
const fn is_invisible(c: char) -> bool {
    matches!(
        c,
        '\u{ad}'
            | '\u{34f}'
            | '\u{61c}'
            | '\u{115f}'..='\u{1160}'
            | '\u{17b4}'..='\u{17b5}'
            | '\u{180b}'..='\u{180f}'
            | '\u{200b}'..='\u{200f}'
            | '\u{202a}'..='\u{202e}'
            | '\u{2060}'..='\u{206f}'
            | '\u{3164}'
            | '\u{fe00}'..='\u{fe0f}'
            | '\u{feff}'
            | '\u{fff9}'..='\u{fffb}'
            | '\u{e0000}'..='\u{e007f}'
    )
}

/// Invisible characters that are part of how text is normally written —
/// joiners inside emoji and scripts, emoji variation selectors, the tag
/// characters of flag sequences — and so no reason on their own to offer a
/// raw form. The explicit escaped view still spells them out.
const fn is_ordinary_invisible(c: char) -> bool {
    matches!(
        c,
        '\u{200c}' | '\u{200d}' | '\u{fe00}'..='\u{fe0f}' | '\u{e0020}'..='\u{e007f}'
    )
}

/// Whether `c` has to be escaped to be seen: anything that is not drawn as
/// itself.
fn needs_escape(c: char) -> bool {
    c.is_control() || (c.is_whitespace() && c != ' ') || is_invisible(c)
}

/// Whether the detail pane should offer the raw view on its own: the
/// command has edge whitespace, or something that has to be escaped to be
/// seen — other than the joiners and selectors ordinary emoji and scripts
/// use. (`^V` shows it for any command.)
pub(super) fn wants_raw(command: &str) -> bool {
    command.starts_with(char::is_whitespace)
        || command.ends_with(char::is_whitespace)
        || command
            .chars()
            .any(|c| needs_escape(c) && !is_ordinary_invisible(c))
}

/// Styles for the raw view's parts.
pub(super) struct RawStyle {
    pub text: Style,
    pub escape: Style,
    pub space: Style,
}

impl RawStyle {
    pub(super) fn from_theme(t: &crate::theme::Theme) -> Self {
        Self {
            text: Style::default().fg(t.text),
            escape: Style::default().fg(t.warning).add_modifier(Modifier::BOLD),
            space: Style::default().fg(t.text_muted),
        }
    }
}

/// The command exactly as stored, in a form where every character can be
/// seen and counted: in quotes, with each space drawn as `·` (a literal `·`
/// is escaped, so a dot always means a space), and `\t`, `\n`, `\r`, `\\`,
/// `\"` and `\u{…}` for everything that does not display as itself —
/// control and non-space whitespace characters, invisible ones such as
/// zero-width spaces, direction overrides and the joiners inside emoji, and
/// a combining mark whose base had to be replaced by a marker or escape.
///
/// A line ends after each `\n`, and lines longer than `width` cells (0: no
/// limit) are broken between graphemes, never at a character that would
/// then be lost: there are no spaces left for a wrap to swallow.
pub(super) fn raw_view(command: &str, width: usize, style: &RawStyle) -> Vec<Line<'static>> {
    use unicode_segmentation::UnicodeSegmentation;

    // (glyph, whether a line ends after it)
    let mut glyphs: Vec<(Glyph, bool)> = vec![(Glyph::new("\"", style.text), false)];
    let escape = |text: &str| (Glyph::new(text, style.escape), false);
    for g in command.graphemes(true) {
        let special = g
            .chars()
            .any(|c| needs_escape(c) || matches!(c, ' ' | '\u{b7}' | '\\' | '"'));
        if !special {
            glyphs.push((Glyph::new(g, style.text), false));
            continue;
        }
        for c in g.chars() {
            glyphs.push(match c {
                ' ' => (Glyph::new("\u{b7}", style.space), false),
                '\n' => (Glyph::new("\\n", style.escape), true),
                '\t' => escape("\\t"),
                '\r' => escape("\\r"),
                '\\' => escape("\\\\"),
                '"' => escape("\\\""),
                c if c == '\u{b7}' || needs_escape(c) => {
                    escape(&format!("\\u{{{:x}}}", u32::from(c)))
                }
                // Split out of its cluster, a combining mark has no base left
                // to draw on — the one it had became a marker or an escape —
                // and a zero-width cell is simply not drawn. Escape it.
                c if unicode_width::UnicodeWidthChar::width(c) == Some(0) => {
                    escape(&format!("\\u{{{:x}}}", u32::from(c)))
                }
                c => (Glyph::new(&c.to_string(), style.text), false),
            });
        }
    }
    glyphs.push((Glyph::new("\"", style.text), false));

    let mut lines: Vec<Vec<Glyph>> = Vec::new();
    let mut line: Vec<Glyph> = Vec::new();
    let mut used = 0;
    for (glyph, breaks) in glyphs {
        if width > 0 && used > 0 && used + glyph.width > width {
            lines.push(std::mem::take(&mut line));
            used = 0;
        }
        used += glyph.width;
        line.push(glyph);
        if breaks {
            lines.push(std::mem::take(&mut line));
            used = 0;
        }
    }
    if !line.is_empty() || lines.is_empty() {
        lines.push(line);
    }
    lines.into_iter().map(to_line).collect()
}

/// Lay pieces out on lines no wider than `width`. A word that would
/// overflow moves to the next line, and the whitespace the break replaces
/// is dropped; a word wider than a whole line is broken between graphemes,
/// so nothing is ever cut off.
fn wrap(pieces: Vec<Piece>, width: usize) -> Vec<Vec<Glyph>> {
    let mut lines: Vec<Vec<Glyph>> = Vec::new();
    let mut line: Vec<Glyph> = Vec::new();
    let mut used = 0;
    let close = |line: &mut Vec<Glyph>, used: &mut usize, lines: &mut Vec<Vec<Glyph>>| {
        while line.last().is_some_and(|g| g.text.trim().is_empty()) {
            line.pop();
        }
        lines.push(std::mem::take(line));
        *used = 0;
    };
    for piece in pieces {
        if piece.is_space {
            if used == 0 && !lines.is_empty() {
                continue;
            }
        } else if used > 0 && used + piece.width() > width {
            close(&mut line, &mut used, &mut lines);
        }
        for glyph in piece.glyphs {
            if used > 0 && used + glyph.width > width {
                close(&mut line, &mut used, &mut lines);
            }
            used += glyph.width;
            line.push(glyph);
        }
    }
    if !line.is_empty() || lines.is_empty() {
        lines.push(line);
    }
    lines
}

/// Everything on one line within `width` cells, ending in `ellipsis` when
/// something had to be cut.
fn truncate(pieces: Vec<Piece>, width: usize, ellipsis: Glyph) -> Vec<Glyph> {
    let glyphs: Vec<Glyph> = pieces.into_iter().flat_map(|p| p.glyphs).collect();
    if glyphs.iter().map(|g| g.width).sum::<usize>() <= width {
        return glyphs;
    }
    let room = width.saturating_sub(ellipsis.width);
    let mut used = 0;
    let mut line: Vec<Glyph> = glyphs
        .into_iter()
        .take_while(|g| {
            used += g.width;
            used <= room
        })
        .collect();
    line.push(ellipsis);
    line
}

/// Merge neighbouring glyphs of the same style into spans.
fn to_line(glyphs: Vec<Glyph>) -> Line<'static> {
    let mut spans: Vec<Span<'static>> = Vec::new();
    let mut text = String::new();
    let mut current: Option<Style> = None;
    for glyph in glyphs {
        if current.is_some_and(|style| style != glyph.style) {
            spans.push(Span::styled(
                std::mem::take(&mut text),
                current.unwrap_or_default(),
            ));
        }
        current = Some(glyph.style);
        text.push_str(&glyph.text);
    }
    if let Some(style) = current {
        spans.push(Span::styled(text, style));
    }
    Line::from(spans)
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
    fn highlighting_follows_each_modes_own_case_rule() {
        // Non-ASCII terms fold with full Unicode lowercasing, as
        // `suvadu_contains_ci` does — including expansions and context.
        assert_eq!(marked("echo İ", "i\u{307}", MatchMode::Terms), ".....İ");
        assert_eq!(marked("echo ΟΣ", "ος", MatchMode::Terms), ".....ΟΣ");
        // ASCII terms, literal and prefix are answered by LIKE, which folds
        // ASCII only: a non-ASCII letter in another case is not a match.
        assert_eq!(marked("echo É é", "é", MatchMode::Literal), ".......é");
        assert_eq!(marked("echo \u{212a}", "k", MatchMode::Terms), "......");
        assert_eq!(marked("Echo x", "ECH", MatchMode::Prefix), "ECH...");
        // fuzzy walks the whole-string Unicode lowercase, like
        // `is_subsequence_ci`.
        assert_eq!(marked("ÉCHO", "éh", MatchMode::Fuzzy), "É.H.");
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
        let a = command_text(&[], "suv status", Fit::Unlimited, &[], &style());
        let b = command_text(&[], "suv status  ", Fit::Unlimited, &[], &style());
        assert_eq!(plain(&a), vec!["suv status"]);
        assert_eq!(plain(&b), vec!["suv status\u{b7}\u{b7}"]);
        let c = command_text(&[], " suv  status", Fit::Unlimited, &[], &style());
        assert_eq!(plain(&c), vec!["\u{b7}suv  status"]);
    }

    /// What a terminal would show: `text` rendered into a real buffer.
    fn rendered(text: Text<'static>, width: u16) -> ratatui::buffer::Buffer {
        use ratatui::widgets::Widget;
        let area = ratatui::layout::Rect::new(0, 0, width, 1);
        let mut buf = ratatui::buffer::Buffer::empty(area);
        ratatui::widgets::Paragraph::new(text).render(area, &mut buf);
        buf
    }

    /// The row as a terminal shows it: a wide character's second cell is
    /// its continuation, not a character of its own.
    fn visible(buf: &ratatui::buffer::Buffer) -> String {
        let mut out = String::new();
        let mut skip = 0;
        for cell in &buf.content {
            if skip > 0 {
                skip -= 1;
                continue;
            }
            out.push_str(cell.symbol());
            skip = super::super::format::display_width(cell.symbol()).saturating_sub(1);
        }
        out.trim_end().to_string()
    }

    #[test]
    fn emphasis_never_splits_a_grapheme() {
        let s = style();
        for (command, query, mode) in [
            // A decomposed accent: `e` plus a combining acute.
            ("echo cafe\u{301}", "e", MatchMode::Terms),
            ("echo cafe\u{301}", "cafe", MatchMode::Literal),
            // A joined emoji, matched on its first scalar.
            (
                "echo \u{1f469}\u{200d}\u{1f4bb}",
                "\u{1f469}",
                MatchMode::Terms,
            ),
            // Wide characters, matched in the middle.
            (
                "echo \u{65e5}\u{672c}\u{8a9e}",
                "\u{672c}",
                MatchMode::Terms,
            ),
        ] {
            let mask = match_mask(command, query, mode);
            let buf = rendered(command_text(&[], command, Fit::Unlimited, &mask, &s), 40);
            assert_eq!(visible(&buf), command, "{command:?} / {query:?}");
        }

        // The grapheme a match touches is emphasised whole.
        let mask = match_mask("echo cafe\u{301}", "cafe", MatchMode::Literal);
        let buf = rendered(
            command_text(&[], "echo cafe\u{301}", Fit::Unlimited, &mask, &s),
            40,
        );
        let accented = buf
            .content
            .iter()
            .find(|c| c.symbol() == "e\u{301}")
            .unwrap();
        assert_eq!(accented.style().fg, s.matched.fg);
    }

    #[test]
    fn line_breaks_and_tabs_inside_a_command_are_marked() {
        let text = command_text(
            &[],
            "for i in 1 2; do\n  echo $i\ndone",
            Fit::Unlimited,
            &[],
            &style(),
        );
        assert_eq!(
            plain(&text),
            vec!["for i in 1 2; do\u{21b5}  echo $i\u{21b5}done"]
        );
    }

    #[test]
    fn every_whitespace_character_is_drawn_so_distinct_commands_differ() {
        let draw = |c: &str| plain(&command_text(&[], c, Fit::Unlimited, &[], &style()));
        for (a, b) in [
            ("printf 'a\tb'", "printf 'a\t\tb'"),
            ("printf 'a\nb'", "printf 'a\n\nb'"),
            ("echo a\u{a0}b", "echo a b"),
        ] {
            assert_ne!(draw(a), draw(b), "{a:?} and {b:?} must not look alike");
        }
        // Long edge runs are counted, not cut to the same ellipsis.
        let twelve = format!("ls{}", " ".repeat(12));
        let thirteen = format!("ls{}", " ".repeat(13));
        assert_ne!(draw(&twelve), draw(&thirteen));
        assert_eq!(
            draw(&twelve),
            vec!["ls\u{b7}\u{b7}\u{b7}\u{b7}\u{b7}\u{b7}\u{b7}\u{b7}+4"]
        );
    }

    /// The raw view as plain text, one string per line.
    fn raw_text(command: &str, width: usize) -> Vec<String> {
        raw_view(command, width, &RawStyle::from_theme(crate::theme::theme()))
            .iter()
            .map(|l| l.spans.iter().map(|s| s.content.as_ref()).collect())
            .collect()
    }

    #[test]
    fn the_raw_view_escapes_what_does_not_draw_and_marks_every_space() {
        assert_eq!(raw_text("git status", 0), vec!["\"git\u{b7}status\""]);
        assert_eq!(
            raw_text("git  status", 0),
            vec!["\"git\u{b7}\u{b7}status\""]
        );
        // A dot always means a space, so a literal one is escaped.
        assert_eq!(
            raw_text("echo a\u{b7}b", 0),
            vec!["\"echo\u{b7}a\\u{b7}b\""]
        );
        assert_eq!(
            raw_text("printf 'a\tb'", 0),
            vec!["\"printf\u{b7}'a\\tb'\""]
        );
        assert_eq!(raw_text("a\\\"b", 0), vec!["\"a\\\\\\\"b\""]);
        // Invisible format characters, and the joiner inside an emoji.
        for (command, escape) in [
            ("echo a\u{200b}b", "\\u{200b}"),
            ("echo \u{202e}txt.exe", "\\u{202e}"),
            ("\u{feff}ls", "\\u{feff}"),
            ("echo co\u{ad}op", "\\u{ad}"),
            ("echo \u{1f469}\u{200d}\u{1f4bb}", "\\u{200d}"),
        ] {
            assert!(raw_text(command, 0)[0].contains(escape), "{command:?}");
        }
        // A combining accent is part of its letter, not an escape.
        assert_eq!(raw_text("cafe\u{301}", 0), vec!["\"cafe\u{301}\""]);
    }

    /// The raw view as a terminal would show it: rendered into a buffer.
    fn raw_rendered(command: &str) -> String {
        use ratatui::widgets::Widget;
        let lines = raw_view(command, 0, &RawStyle::from_theme(crate::theme::theme()));
        let area = ratatui::layout::Rect::new(0, 0, 60, 1);
        let mut buf = ratatui::buffer::Buffer::empty(area);
        ratatui::widgets::Paragraph::new(lines).render(area, &mut buf);
        visible(&buf)
    }

    #[test]
    fn a_combining_mark_split_from_its_base_is_escaped_not_lost() {
        // A space carrying a combining accent: the space becomes `·`, and the
        // accent, left without a base to draw on, must still be seen.
        let acute = raw_rendered("echo \u{301}x");
        let circumflex = raw_rendered("echo \u{302}x");
        assert_ne!(acute, circumflex, "two different commands look alike");
        assert_eq!(acute, "\"echo\u{b7}\\u{301}x\"");
        assert_eq!(circumflex, "\"echo\u{b7}\\u{302}x\"");
        // The same after a quote or backslash, which are escaped too.
        assert_eq!(raw_rendered("a\"\u{301}"), "\"a\\\"\\u{301}\"");
        // An accented letter that needs no escaping keeps drawing as itself.
        assert_eq!(
            raw_rendered("caf\u{e9} cafe\u{301}"),
            "\"caf\u{e9}\u{b7}cafe\u{301}\""
        );
    }

    #[test]
    fn the_raw_view_breaks_after_each_line_break_and_wraps_without_loss() {
        assert_eq!(
            raw_text("for i in 1; do\n  echo $i\ndone", 0),
            vec![
                "\"for\u{b7}i\u{b7}in\u{b7}1;\u{b7}do\\n",
                "\u{b7}\u{b7}echo\u{b7}$i\\n",
                "done\"",
            ]
        );
        // One space or two before `b`, right at a wrap point: the two must
        // stay different, and nothing may be dropped at the break.
        let one = format!("echo {} b", "a".repeat(77));
        let two = format!("echo {}  b", "a".repeat(77));
        let (a, b) = (raw_text(&one, 78), raw_text(&two, 78));
        assert_ne!(a, b);
        assert_eq!(a.concat(), raw_text(&one, 0).concat());
        assert_eq!(b.concat(), raw_text(&two, 0).concat());
        assert!(a
            .iter()
            .all(|l| super::super::format::display_width(l) <= 78));
    }

    #[test]
    fn a_raw_line_is_offered_only_when_the_display_is_not_the_text() {
        assert!(!wants_raw("git status"));
        assert!(!wants_raw("git  status"), "spaces draw as themselves");
        assert!(!wants_raw(
            "echo \u{1f469}\u{200d}\u{1f4bb} \u{2764}\u{fe0f}"
        ));
        for command in [
            "printf 'a\tb'",
            "suv status  ",
            " ls",
            "a\nb",
            "echo a\u{a0}b",
            "echo a\u{200b}b",
            "echo \u{202e}txt",
        ] {
            assert!(wants_raw(command), "{command:?}");
        }
    }

    #[test]
    fn matched_characters_take_the_emphasis_style() {
        let s = style();
        let mask = match_mask("git checkout", "check", MatchMode::Terms);
        let text = command_text(&[], "git checkout", Fit::Unlimited, &mask, &s);
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
        let text = command_text(
            &prefix,
            "docker compose up --build web",
            Fit::Wrap(16),
            &[],
            &style(),
        );
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
