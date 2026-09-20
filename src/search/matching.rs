//! Explicit matching modes for recall.
//!
//! **Matching** decides *which* entries are eligible. **Ranking** decides the
//! order eligible entries appear in. They are deliberately separate: changing
//! the ranking (`^S` smart/recent, `^U` unique) never changes the set of
//! matches, and changing the matching mode never reorders within a mode.
//!
//! The rules, identical in the CLI, the TUI and this module's
//! [`MatchMode::matches`] predicate:
//!
//! | Mode      | Rule                                                             |
//! |-----------|------------------------------------------------------------------|
//! | `terms`   | every whitespace-separated term must appear as a substring (AND) |
//! | `literal` | the whole query must appear as one contiguous substring          |
//! | `prefix`  | the entry must *start with* the query                            |
//! | `fuzzy`   | the query's characters must appear in order, gaps allowed        |
//!
//! Shared rules:
//!
//! * **Terms are ANDed, never ORed.** Order does not matter in `terms` mode;
//!   entries whose terms appear contiguously and in query order rank higher,
//!   but a scattered match is still a match.
//! * **Case:** matching is case-insensitive for ASCII. Non-ASCII letters are
//!   compared case-sensitively, because the underlying SQL `LIKE` only folds
//!   ASCII — `README` finds `readme`, `ÉCHO` does not find `écho`.
//! * **Quoting:** there is no quoting syntax. `"` and `'` are ordinary
//!   characters that must appear in the entry. To match a phrase that contains
//!   spaces or punctuation exactly, use `literal` mode.
//! * **Punctuation:** never stripped, never tokenised. `git-push` is one term
//!   and only matches entries containing `git-push`. SQL wildcards (`%`, `_`)
//!   are escaped, so they match themselves.
//! * **Whitespace:** a query is trimmed; runs of internal whitespace separate
//!   terms in `terms` mode and are matched literally in `literal` mode.

/// How a typed query is turned into a set of eligible entries.
#[derive(
    Clone,
    Copy,
    Debug,
    Default,
    PartialEq,
    Eq,
    clap::ValueEnum,
    serde::Serialize,
    serde::Deserialize,
)]
#[serde(rename_all = "kebab-case")]
#[clap(rename_all = "kebab-case")]
pub enum MatchMode {
    /// Every whitespace-separated term must appear as a substring, in any
    /// order. This is the historical default and stays the default.
    #[default]
    Terms,
    /// The whole query must appear as one contiguous substring.
    Literal,
    /// The entry must start with the query.
    Prefix,
    /// The query's characters must appear in order, with gaps allowed
    /// (`gco` finds `git checkout`). Opt-in.
    Fuzzy,
}

impl MatchMode {
    /// Stable lower-case name used in the UI, `--match` and config.
    pub const fn label(self) -> &'static str {
        match self {
            Self::Terms => "terms",
            Self::Literal => "literal",
            Self::Prefix => "prefix",
            Self::Fuzzy => "fuzzy",
        }
    }

    /// One-line rule, shown in help and in the no-results state.
    pub const fn describe(self) -> &'static str {
        match self {
            Self::Terms => "every word must appear, any order",
            Self::Literal => "the whole query must appear as typed",
            Self::Prefix => "the command must start with the query",
            Self::Fuzzy => "letters in order, gaps allowed",
        }
    }

    /// Cycle order for the in-TUI mode key.
    pub const fn next(self) -> Self {
        match self {
            Self::Terms => Self::Literal,
            Self::Literal => Self::Prefix,
            Self::Prefix => Self::Fuzzy,
            Self::Fuzzy => Self::Terms,
        }
    }

    /// `true` when the database can narrow candidates for this mode, so the
    /// whole history is searched rather than a recent window.
    pub const fn narrows_in_sql(self) -> bool {
        !matches!(self, Self::Fuzzy)
    }

    /// The documented predicate for this mode. Mirrors the SQL exactly,
    /// including ASCII-only case folding.
    pub fn matches(self, haystack: &str, query: &str) -> bool {
        let q = query.trim();
        if q.is_empty() {
            return true;
        }
        let hay = haystack.to_ascii_lowercase();
        let needle = q.to_ascii_lowercase();
        match self {
            Self::Terms => query_terms(&needle).iter().all(|t| hay.contains(t)),
            Self::Literal => hay.contains(&needle),
            Self::Prefix => hay.starts_with(&needle),
            Self::Fuzzy => is_subsequence(&hay, &needle),
        }
    }
}

/// Split a query into terms. Whitespace only — there is no quoting syntax, so
/// query interpretation is unchanged from before explicit modes existed.
pub fn query_terms(query: &str) -> Vec<&str> {
    query.split_whitespace().collect()
}

/// `true` when every char of `needle` appears in `hay`, in order.
fn is_subsequence(hay: &str, needle: &str) -> bool {
    let mut chars = hay.chars();
    needle.chars().all(|c| chars.any(|h| h == c))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn literal_and_fuzzy_disagree_on_the_documented_example() {
        // The example the docs promise: an abbreviation is a fuzzy match but
        // never a literal one.
        assert!(!MatchMode::Literal.matches("git checkout main", "gco"));
        assert!(MatchMode::Fuzzy.matches("git checkout main", "gco"));
    }

    #[test]
    fn terms_are_anded_and_order_insensitive() {
        assert!(MatchMode::Terms.matches("git commit --amend", "amend git"));
        assert!(MatchMode::Terms.matches("git commit --amend", "git amend"));
        // Missing term → no match. Terms are never ORed.
        assert!(!MatchMode::Terms.matches("git commit --amend", "git rebase"));
    }

    #[test]
    fn literal_requires_the_whole_query_contiguously() {
        assert!(MatchMode::Literal.matches("docker compose up -d", "compose up"));
        // The words are present but not adjacent.
        assert!(!MatchMode::Literal.matches("docker compose up -d", "docker up"));
        assert!(MatchMode::Terms.matches("docker compose up -d", "docker up"));
    }

    #[test]
    fn prefix_anchors_at_the_start() {
        assert!(MatchMode::Prefix.matches("cargo test --offline", "cargo t"));
        assert!(!MatchMode::Prefix.matches("cargo test --offline", "test"));
        assert!(MatchMode::Terms.matches("cargo test --offline", "test"));
    }

    #[test]
    fn case_folds_ascii_only_as_documented() {
        assert!(MatchMode::Literal.matches("cat README.md", "readme"));
        assert!(MatchMode::Literal.matches("cat readme.md", "README"));
        // Non-ASCII is compared case-sensitively — documented, not accidental.
        assert!(!MatchMode::Literal.matches("echo écho", "ÉCHO"));
        assert!(MatchMode::Literal.matches("echo écho", "écho"));
    }

    #[test]
    fn quotes_are_ordinary_characters_not_syntax() {
        // If quoting were special, this would match; it must not, because the
        // pre-existing interpretation treats `"` as a character to find.
        assert!(!MatchMode::Terms.matches("git commit -m fix", "\"git commit\""));
        assert!(MatchMode::Terms.matches("git commit -m \"fix\"", "\"fix\""));
    }

    #[test]
    fn punctuation_is_never_stripped_or_tokenised() {
        assert!(MatchMode::Terms.matches("git-push --force", "git-push"));
        // `git-push` is one term, so it does not match a space-separated form.
        assert!(!MatchMode::Terms.matches("git push --force", "git-push"));
    }

    #[test]
    fn empty_query_matches_everything() {
        assert!(MatchMode::Terms.matches("anything", ""));
        assert!(MatchMode::Fuzzy.matches("anything", "   "));
    }

    #[test]
    fn terms_splits_on_whitespace_runs_only() {
        assert_eq!(query_terms("  git   add  "), vec!["git", "add"]);
        assert_eq!(query_terms(""), Vec::<&str>::new());
    }

    #[test]
    fn default_mode_is_terms_so_upgrades_do_not_reinterpret_queries() {
        assert_eq!(MatchMode::default(), MatchMode::Terms);
    }

    #[test]
    fn mode_cycle_visits_every_mode_and_returns_to_the_default() {
        let mut seen = vec![MatchMode::default()];
        let mut m = MatchMode::default();
        for _ in 0..3 {
            m = m.next();
            seen.push(m);
        }
        assert_eq!(
            seen,
            vec![
                MatchMode::Terms,
                MatchMode::Literal,
                MatchMode::Prefix,
                MatchMode::Fuzzy
            ]
        );
        assert_eq!(m.next(), MatchMode::Terms);
    }

    #[test]
    fn only_fuzzy_cannot_be_narrowed_by_the_database() {
        assert!(MatchMode::Terms.narrows_in_sql());
        assert!(MatchMode::Literal.narrows_in_sql());
        assert!(MatchMode::Prefix.narrows_in_sql());
        assert!(!MatchMode::Fuzzy.narrows_in_sql());
    }

    #[test]
    fn labels_are_stable_and_round_trip_through_clap() {
        use clap::ValueEnum;
        for m in MatchMode::value_variants() {
            let label = m.label();
            assert_eq!(
                MatchMode::from_str(label, true).unwrap(),
                *m,
                "label {label} must parse back"
            );
        }
    }
}
