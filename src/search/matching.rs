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
//! * **Terms are combined with AND, never OR.** Order does not matter in
//!   `terms` mode;
//!   entries whose terms appear contiguously and in query order rank higher,
//!   but a scattered match is still a match.
//! * **Case:** matching is always case-insensitive for ASCII, and how far it
//!   goes beyond that depends on the mode. `terms` and `fuzzy` are re-ranked
//!   in memory after SQL narrowing, so they fold case with Rust's full
//!   Unicode lowercasing — a non-ASCII term also narrows through
//!   `suvadu_contains_ci()` rather than `LIKE`, so `ÉCHO` does find `écho`.
//!   `literal` and `prefix` are answered by SQL alone, and the underlying
//!   `LIKE` folds ASCII only, so in those two modes `ÉCHO` does not find
//!   `écho`.
//!   [`MatchMode::matches`] below is the ASCII-only reference predicate used
//!   by tests and by `fuzzy`'s subsequence check; it is not the live filter
//!   for `terms`.
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

    /// The documented predicate for this mode, folding ASCII case only.
    /// This mirrors what `literal` and `prefix` do in SQL exactly. It is the
    /// reference the tests check against, and the subsequence check `fuzzy`
    /// applies after SQL narrowing — it is *not* the live filter for `terms`,
    /// which folds non-ASCII case too (see the module docs).
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

/// How a mode asks the database for candidates.
///
/// Every mode narrows in SQL, so a match is found however old it is and
/// however large the history has grown — the database never hands back "the
/// newest N rows and hope". The narrowing is always a *superset* of the mode's
/// true match set; the in-memory pass then applies the exact rule.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct QueryPlan {
    /// Whole-query clause (`QueryFilter::query`), for literal and prefix.
    pub query: Option<String>,
    /// Anchor the whole-query clause at the start (`QueryFilter::prefix_match`).
    pub prefix: bool,
    /// Per-token clauses, combined with AND (`QueryFilter::query_tokens`).
    pub tokens: Vec<String>,
    /// Whether the in-memory scorer reorders what SQL returned.
    ///
    /// `false` for literal and prefix: their match set is exactly what SQL
    /// returns, so results stay in recency order and the mode is predictable.
    /// `true` for terms and fuzzy, where relevance ordering is the point.
    pub rerank: bool,
    /// Whether the scorer may keep a pure subsequence match (an abbreviation).
    /// Only `fuzzy` says yes; that is the whole difference between it and the
    /// default.
    pub allow_subsequence: bool,
}

impl MatchMode {
    /// Turn a typed query into the database request for this mode.
    pub fn plan(self, query: &str) -> QueryPlan {
        let q = query.trim();
        if q.is_empty() {
            return QueryPlan::default();
        }
        match self {
            Self::Terms => QueryPlan {
                tokens: query_terms(q).into_iter().map(str::to_string).collect(),
                rerank: true,
                ..QueryPlan::default()
            },
            Self::Literal => QueryPlan {
                query: Some(q.to_string()),
                ..QueryPlan::default()
            },
            Self::Prefix => QueryPlan {
                query: Some(q.to_string()),
                prefix: true,
                ..QueryPlan::default()
            },
            // A subsequence match must contain every character of the query,
            // so "each distinct character appears somewhere" is a sound
            // superset — narrow enough for SQL to do real work, never so
            // narrow that it hides a true fuzzy match.
            Self::Fuzzy => QueryPlan {
                tokens: distinct_chars(q),
                rerank: true,
                allow_subsequence: true,
                ..QueryPlan::default()
            },
        }
    }
}

/// The distinct characters of `q` (whitespace dropped), each as its own token.
fn distinct_chars(q: &str) -> Vec<String> {
    let mut seen = Vec::new();
    for c in q.chars().filter(|c| !c.is_whitespace()) {
        let lower = c.to_lowercase().to_string();
        if !seen.contains(&lower) {
            seen.push(lower);
        }
    }
    seen
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
    fn terms_mode_asks_sql_for_one_clause_per_term() {
        let plan = MatchMode::Terms.plan("  git   rebase ");
        assert_eq!(plan.tokens, vec!["git".to_string(), "rebase".to_string()]);
        assert_eq!(plan.query, None);
        assert!(!plan.prefix);
        assert!(plan.rerank);
        assert!(!plan.allow_subsequence);
    }

    #[test]
    fn literal_mode_asks_sql_for_the_whole_query_and_keeps_recency_order() {
        let plan = MatchMode::Literal.plan(" docker compose up ");
        assert_eq!(plan.query.as_deref(), Some("docker compose up"));
        assert!(plan.tokens.is_empty());
        assert!(!plan.prefix);
        // Literal is the predictable mode: SQL already returns exactly the
        // match set, so nothing reorders it.
        assert!(!plan.rerank);
    }

    #[test]
    fn prefix_mode_anchors_the_whole_query() {
        let plan = MatchMode::Prefix.plan("cargo t");
        assert_eq!(plan.query.as_deref(), Some("cargo t"));
        assert!(plan.prefix);
        assert!(plan.tokens.is_empty());
        assert!(!plan.rerank);
    }

    #[test]
    fn fuzzy_narrows_by_distinct_characters_so_old_matches_are_not_lost() {
        let plan = MatchMode::Fuzzy.plan("gco");
        assert_eq!(
            plan.tokens,
            vec!["g".to_string(), "c".to_string(), "o".to_string()]
        );
        assert!(plan.allow_subsequence);
        assert!(plan.rerank);
    }

    #[test]
    fn fuzzy_character_narrowing_never_excludes_a_real_subsequence_match() {
        // The SQL superset must admit every entry the mode would accept.
        let corpus = [
            "git checkout main",
            "gcc -o out main.c",
            "echo going",
            "ls -la",
        ];
        for query in ["gco", "gcm", "gc", "g c o"] {
            let plan = MatchMode::Fuzzy.plan(query);
            for cmd in corpus {
                let lc = cmd.to_ascii_lowercase();
                let sql_admits = plan.tokens.iter().all(|t| lc.contains(t.as_str()));
                if MatchMode::Fuzzy.matches(cmd, query) {
                    assert!(
                        sql_admits,
                        "{query:?} matches {cmd:?} but SQL narrowing would drop it"
                    );
                }
            }
        }
    }

    #[test]
    fn fuzzy_tokens_are_deduplicated_and_ignore_whitespace() {
        assert_eq!(
            MatchMode::Fuzzy.plan("a a  B").tokens,
            vec!["a".to_string(), "b".to_string()]
        );
    }

    #[test]
    fn an_empty_query_narrows_nothing_in_any_mode() {
        for mode in [
            MatchMode::Terms,
            MatchMode::Literal,
            MatchMode::Prefix,
            MatchMode::Fuzzy,
        ] {
            assert_eq!(mode.plan("   "), QueryPlan::default(), "{mode:?}");
        }
    }

    #[test]
    fn only_fuzzy_admits_a_pure_subsequence() {
        for mode in [MatchMode::Terms, MatchMode::Literal, MatchMode::Prefix] {
            assert!(!mode.plan("gco").allow_subsequence, "{mode:?}");
        }
        assert!(MatchMode::Fuzzy.plan("gco").allow_subsequence);
    }

    #[test]
    fn the_default_mode_plans_exactly_the_pre_existing_token_narrowing() {
        // Before explicit modes, interactive search split the query on
        // whitespace and required every token as a substring. The default
        // mode must still do precisely that, or upgrading reinterprets
        // everyone's queries.
        let query = "git commit --amend";
        let plan = MatchMode::default().plan(query);
        let legacy: Vec<String> = query.split_whitespace().map(str::to_string).collect();
        assert_eq!(plan.tokens, legacy);
        assert_eq!(plan.query, None);
        assert!(!plan.prefix);
        assert!(!plan.allow_subsequence);
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
