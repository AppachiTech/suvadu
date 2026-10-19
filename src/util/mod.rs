mod cleanup;
mod exclusion;
mod file;
mod format;
mod git;
mod highlight;
mod reconcile_key;
mod terminal;
mod timestamp;
mod wrap;

pub use cleanup::*;
pub use exclusion::*;
pub use file::*;
pub use format::*;
pub use git::*;
pub use highlight::*;
pub use reconcile_key::*;
pub use terminal::*;
pub use timestamp::*;
pub use wrap::*;

use std::sync::LazyLock;

// ── Cached project directories ──────────────────────────────

#[cfg(not(test))]
static PROJECT_DIRS: LazyLock<Option<directories::ProjectDirs>> =
    LazyLock::new(|| directories::ProjectDirs::from("tech", "appachi", "suvadu"));

// Unit tests must never read or write a developer's real app data/config.
#[cfg(test)]
static TEST_ROOT: LazyLock<tempfile::TempDir> = LazyLock::new(|| {
    tempfile::Builder::new()
        .prefix("suvadu-tests-")
        .tempdir()
        .expect("test app directory")
});
#[cfg(test)]
static PROJECT_DIRS: LazyLock<Option<directories::ProjectDirs>> =
    LazyLock::new(|| directories::ProjectDirs::from_path(TEST_ROOT.path().to_path_buf()));

/// Cached project directory lookup. Avoids re-computing paths on every call.
/// Called from config, db, hooks, integrations, and alias modules.
pub fn project_dirs() -> Option<&'static directories::ProjectDirs> {
    PROJECT_DIRS.as_ref()
}

// ── Case folding ───────────────────────────────────────

/// Append `c`'s case fold to `out` and return how many characters that was.
///
/// The fold is Rust's Unicode lowercase of the letter on its own, plus the
/// folds lowercasing does not make, from Unicode's `CaseFolding.txt`:
///
/// * final sigma `ς` is `σ`, so `ΟΔΟΣ`, `οδος` and `σ` agree;
/// * `ß` and capital `ẞ` are `ss`, so `Straße` finds `STRASSE`;
/// * the Latin ligatures `ﬀ ﬁ ﬂ ﬃ ﬄ ﬅ ﬆ` are their letters, the long `ſ` is
///   `s`, and `ŉ` is `ʼn`;
/// * the Greek symbol forms `ϐ ϑ ϕ ϖ ϰ ϱ ϵ` are `β θ φ π κ ρ ε`, and the
///   combining iota subscript is `ι`.
///
/// These are the folds a command line plausibly holds. Other full folds that
/// only decompose a precomposed letter (`ΐ`, `ǰ`, Armenian `և`, …) fall back
/// to lowercasing, which leaves them as they are, so they still match
/// themselves in either case.
///
/// No locale rules apply. Turkish `İ` folds to `i` followed by U+0307
/// COMBINING DOT ABOVE, as Unicode's default folding has it, so `İstanbul`
/// is found by `İstanbul` and `i̇stanbul` but not by `istanbul`; dotless `ı`
/// is its own letter and matches neither `i` nor `I`.
///
/// A letter folds the same wherever it stands, unlike `str::to_lowercase`,
/// which picks `ς` or `σ` by position: so a text's fold is its letters'
/// folds, one after another, and the highlighter can credit each folded
/// character to the letter it came from.
pub fn push_folded(out: &mut String, c: char) -> usize {
    let special: Option<&str> = match c {
        '\u{df}' | '\u{1e9e}' => Some("ss"),
        '\u{3c2}' => Some("\u{3c3}"),
        '\u{17f}' => Some("s"),
        '\u{149}' => Some("\u{2bc}n"),
        '\u{fb00}' => Some("ff"),
        '\u{fb01}' => Some("fi"),
        '\u{fb02}' => Some("fl"),
        '\u{fb03}' => Some("ffi"),
        '\u{fb04}' => Some("ffl"),
        '\u{fb05}' | '\u{fb06}' => Some("st"),
        '\u{3d0}' => Some("\u{3b2}"),
        '\u{3d1}' => Some("\u{3b8}"),
        '\u{3d5}' => Some("\u{3c6}"),
        '\u{3d6}' => Some("\u{3c0}"),
        '\u{3f0}' => Some("\u{3ba}"),
        '\u{3f1}' => Some("\u{3c1}"),
        '\u{3f5}' => Some("\u{3b5}"),
        '\u{345}' => Some("\u{3b9}"),
        _ => None,
    };
    if let Some(folded) = special {
        out.push_str(folded);
        return folded.chars().count();
    }
    // On its own Σ lowercases to σ; only `str::to_lowercase` makes ς.
    let lower = c.to_lowercase();
    let count = lower.len();
    out.extend(lower);
    count
}

/// `text` case-folded (see [`push_folded`]): the form two texts are compared
/// in when case must not matter beyond ASCII — `terms`' non-ASCII words
/// (`suvadu_contains_ci`), `fuzzy` ([`is_subsequence_ci`]), recall's ranking
/// and its highlighting.
pub fn fold_case(text: &str) -> String {
    // Most commands are ASCII, and every database row a non-ASCII term is
    // tested against passes through here.
    if text.is_ascii() {
        return text.to_ascii_lowercase();
    }
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        push_folded(&mut out, c);
    }
    out
}

/// Whether `haystack`, case-folded, contains `folded_needle`, which the
/// caller has folded already ([`fold_case`]) — the `suvadu_contains_ci`
/// rule, tested on every row a non-ASCII term is matched against.
///
/// An ASCII haystack folds to ASCII, so it cannot contain a needle that
/// folds to anything else; most commands are ASCII, and this answers them
/// without folding at all.
pub fn contains_folded(haystack: &str, folded_needle: &str) -> bool {
    if haystack.is_ascii() {
        folded_needle.is_ascii() && haystack.to_ascii_lowercase().contains(folded_needle)
    } else {
        fold_case(haystack).contains(folded_needle)
    }
}

// ── Subsequence matching ───────────────────────────────

/// `true` when every character of `needle` occurs in `haystack` in order,
/// gaps allowed, comparing both case-folded ([`fold_case`]) — the same
/// folding `terms` gets from `suvadu_contains_ci`, so `ÉCHO` finds `écho` and
/// `straße` finds `STRASSE` in either mode. An empty (or whitespace-only)
/// needle matches everything.
///
/// This is the single definition of the `fuzzy` recall mode's rule. It lives
/// here rather than in `search::matching` because the database needs it too:
/// `db::register_subseq_ci` exposes it to SQL as `suvadu_subseq_ci()` so a
/// fuzzy query can be matched and counted completely in the database, while
/// `MatchMode::matches` answers with the very same function in memory.
pub fn is_subsequence_ci(haystack: &str, needle: &str) -> bool {
    let needle = needle.trim();
    if needle.is_empty() {
        return true;
    }
    let hay = fold_case(haystack);
    let mut chars = hay.chars();
    fold_case(needle).chars().all(|c| chars.any(|h| h == c))
}

// ── Separator-insensitive matching ─────────────────────

/// The characters separator-insensitive matching ignores: whitespace that
/// separates words, and the punctuation that joins them in identifiers,
/// paths and hostnames (`hive-agent`, `hive_agent`, `hive.agent`,
/// `hive/agent`, `host:port`).
pub const SEPARATORS: [char; 8] = [' ', '\t', '\n', '-', '_', '.', '/', ':'];

/// `text` with every [`SEPARATORS`] character removed and ASCII letters
/// lower-cased, so `Hive-Agent`, `hive_agent` and `hiveagent` all become
/// `hiveagent`.
///
/// Only ASCII case is folded because this is, character for character, the
/// SQL expression behind the `entries_norm_fts` index (see
/// `db::normalized_sql`), and `SQLite`'s `lower()` folds ASCII only. Keeping
/// the two identical is what lets the index, the in-memory ranker and the
/// highlighter agree on what a separator-insensitive match is.
pub fn normalize_separators(text: &str) -> String {
    text.chars()
        .filter(|c| !SEPARATORS.contains(c))
        .map(|c| c.to_ascii_lowercase())
        .collect()
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
    fn fold_case_folds_what_lowercasing_misses() {
        for (text, folded) in [
            // Final sigma is the same letter as σ; lowercasing keeps ς.
            ("ΟΔΟΣ", "οδοσ"),
            ("οδος", "οδοσ"),
            ("ς", "σ"),
            // ß and capital ẞ fold to "ss", as uppercase SS does.
            ("Straße", "strasse"),
            ("STRAẞE", "strasse"),
            ("STRASSE", "strasse"),
            // Ligatures and the long s.
            ("\u{fb01}le", "file"),
            ("\u{fb00}\u{fb03}\u{fb06}", "ffffist"),
            ("\u{17f}", "s"),
            // Greek symbol forms of ordinary letters.
            (
                "\u{3d0}\u{3d1}\u{3d5}\u{3d6}\u{3f0}\u{3f1}\u{3f5}",
                "βθφπκρε",
            ),
            ("\u{212a}elvin", "kelvin"),
            ("ÉCHO", "écho"),
            // Turkish: no locale rules. İ folds to i + combining dot above,
            // as Unicode's default folding has it; dotless ı stays itself.
            ("İ", "i\u{307}"),
            ("ı", "ı"),
            ("I", "i"),
            ("plain ascii 123", "plain ascii 123"),
        ] {
            assert_eq!(fold_case(text), folded, "{text:?}");
        }
    }

    #[test]
    fn fold_case_is_the_same_letter_by_letter() {
        // Folding has no context, so a text folds to its letters' folds —
        // what lets the highlighter credit each folded letter to its source.
        for text in ["ΟΔΟΣ οδος", "Straße STRAẞE", "İı\u{fb03}", "a\u{301}"] {
            let mut joined = String::new();
            let mut counts = 0;
            for c in text.chars() {
                counts += push_folded(&mut joined, c);
            }
            assert_eq!(joined, fold_case(text), "{text:?}");
            assert_eq!(counts, joined.chars().count(), "{text:?}");
        }
    }

    #[test]
    fn contains_folded_answers_ascii_rows_without_folding_them() {
        // A needle that folds to ASCII can still be in an ASCII row...
        assert!(contains_folded("cd STRASSE", &fold_case("straße")));
        assert!(contains_folded("cd Strasse", &fold_case("\u{17f}")));
        // ...and one that does not, never.
        assert!(!contains_folded("echo Istanbul", &fold_case("İstanbul")));
        assert!(!contains_folded("echo odos", &fold_case("σ")));
        // Non-ASCII rows are folded.
        assert!(contains_folded("echo ΟΔΟΣ", &fold_case("ς")));
        assert!(contains_folded("cd Straße", &fold_case("STRASSE")));
        assert!(contains_folded("anything", ""));
    }

    #[test]
    fn subsequences_fold_case_fully() {
        assert!(is_subsequence_ci("echo ΟΔΟΣ", "σ"));
        assert!(is_subsequence_ci("echo οδος", "ΟΣ"));
        assert!(is_subsequence_ci("Straße", "strasse"));
        assert!(is_subsequence_ci("STRASSE", "straße"));
        assert!(is_subsequence_ci("Straße", "sss"));
        assert!(!is_subsequence_ci("Strase", "straße"));
    }

    #[test]
    fn normalize_separators_drops_each_separator_and_folds_ascii_case() {
        for spelling in [
            "hive-agent",
            "hive_agent",
            "hive.agent",
            "hive/agent",
            "hive:agent",
            "hive agent",
            "hive\tagent",
            "hive\nagent",
            "Hive-Agent",
            "HIVEAGENT",
            "-hive--agent_",
        ] {
            assert_eq!(normalize_separators(spelling), "hiveagent", "{spelling:?}");
        }
        // Everything else is kept as it is.
        assert_eq!(normalize_separators("a%b\\c\"d'e"), "a%b\\c\"d'e");
        assert_eq!(normalize_separators("--._/: \t\n"), "");
    }

    #[test]
    fn normalize_separators_folds_ascii_case_only() {
        // SQLite's lower() leaves non-ASCII letters alone, so this must too.
        assert_eq!(normalize_separators("ÉCHO-Émile"), "ÉchoÉmile");
        assert_eq!(normalize_separators("\u{212a}elvin"), "\u{212a}elvin");
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
}
