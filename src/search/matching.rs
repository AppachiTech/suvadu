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
//! | `terms`   | every whitespace-separated term must appear as a substring (AND); if none does, see *Separators* below |
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
//!   goes beyond that depends on the mode. `terms` and `fuzzy` fold case with
//!   Rust's full Unicode lowercasing, so `ÉCHO` does find `écho` in both: a
//!   non-ASCII term narrows through `suvadu_contains_ci()` rather than
//!   `LIKE`, and `fuzzy` is decided by `suvadu_subseq_ci()`, which is
//!   [`is_subsequence_ci`] itself. `literal` and `prefix` fold ASCII only,
//!   because they are answered by `LIKE`.
//!   [`MatchMode::matches`] below is the reference predicate used by tests
//!   and, for `fuzzy`, the live rule. For `terms` it folds ASCII only, so it
//!   is not the live filter there — that one folds more case than it does.
//! * **Quoting:** there is no quoting syntax. `"` and `'` are ordinary
//!   characters that must appear in the entry. To match a phrase that contains
//!   spaces or punctuation exactly, use `literal` mode.
//! * **Punctuation:** never stripped, never tokenised. `git-push` is one term
//!   and only matches entries containing `git-push`. SQL wildcards (`%`, `_`)
//!   are escaped, so they match themselves.
//! * **Separators — the one fallback:** when a `terms` query matches
//!   *nothing* in the current scope and filters, it is asked again with
//!   separators ignored — spaces, tabs, newlines, `-`, `_`, `.`, `/` and `:`
//!   ([`normalize_separators`]) — on both the terms and the commands, so
//!   `hiveagent` finds `hive-agent` and `hive_agent`, and `hive-agent` finds
//!   `hiveagent`. The results are labelled as such wherever they are shown.
//!   It never mixes with strict results: one exact hit and only exact hits
//!   are shown, so a count and its pages always come from one rule. Each
//!   term still has to appear (AND); a term made only of separators is
//!   dropped. `literal`, `prefix` and `fuzzy` have no fallback.
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
    /// order. When nothing matches, the query is retried ignoring
    /// separators (-_./: and whitespace), and those results are labelled.
    /// This is the historical default and stays the default.
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

    /// The documented predicate for this mode. `literal`, `prefix` and
    /// `terms` fold ASCII case only, which mirrors what `literal` and `prefix`
    /// do in SQL exactly; `fuzzy` folds Unicode case, because it *is* the
    /// subsequence check the database applies. It is the reference the tests
    /// check against — and *not* the live filter for `terms`, which folds
    /// non-ASCII case too (see the module docs).
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
            Self::Fuzzy => is_subsequence_ci(&hay, &needle),
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
/// newest N rows and hope".
///
/// The clauses below are a *superset* of the mode's true match set on their
/// own: `fuzzy` asks only that each distinct query character occur somewhere.
/// Recall therefore pairs them with the mode's exact rule, also in SQL
/// (`FilterBuilder::with_subsequence`, i.e. `suvadu_subseq_ci`), so the
/// database decides eligibility completely. That is what makes a result count
/// trustworthy and every page of it reachable; the in-memory pass only ranks.
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
    /// Whether this mode's eligibility rule is the whole-query subsequence
    /// (an abbreviation like `gco` → `git checkout`) rather than "every token
    /// appears literally". Only `fuzzy` says yes; that is the whole difference
    /// between it and the default. It selects the rule outright — it is not a
    /// fallback applied only when nothing matched literally.
    pub allow_subsequence: bool,
    /// Separator-insensitive per-term clauses, combined with AND: each is a
    /// query term passed through [`normalize_separators`], to be found in
    /// the command with its separators removed (the `entries_norm_fts`
    /// index). Only [`MatchMode::loose_plan`] fills this in, and a plan that
    /// has them has nothing else narrowing it, so the whole result set comes
    /// from this one rule.
    pub loose_tokens: Vec<String>,
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
            // The subsequence predicate alone decides eligibility. Narrowing
            // first with one `LIKE '%c%'` per letter cost more than it saved:
            // a single character has no trigram to look up, so every one of
            // those clauses tested the whole history on its own.
            Self::Fuzzy => QueryPlan {
                rerank: true,
                allow_subsequence: true,
                ..QueryPlan::default()
            },
        }
    }
}

impl MatchMode {
    /// The separator-insensitive retry of a query that matched nothing, or
    /// `None` when this mode has no such retry or the query has nothing left
    /// once its separators are gone.
    ///
    /// Only `terms` has one. Its terms are split on whitespace as usual and
    /// each is normalized like the commands are (separators removed, ASCII
    /// lower-cased); a term that was nothing but separators is dropped. Any
    /// entry the strict plan accepts, this one accepts too, which is why it
    /// is asked only when the strict plan found nothing.
    pub fn loose_plan(self, query: &str) -> Option<QueryPlan> {
        if self != Self::Terms {
            return None;
        }
        let loose_tokens: Vec<String> = query_terms(query.trim())
            .into_iter()
            .map(normalize_separators)
            .filter(|t| !t.is_empty())
            .collect();
        (!loose_tokens.is_empty()).then(|| QueryPlan {
            loose_tokens,
            rerank: true,
            ..QueryPlan::default()
        })
    }
}

/// Whether every one of `loose_tokens` (already normalized, as
/// [`MatchMode::loose_plan`] makes them) appears in `haystack` once its
/// separators are removed — the separator-insensitive rule, as the in-memory
/// ranker and the tests apply it.
///
/// Case is folded with Rust's Unicode lowercasing, so this accepts every
/// entry the database does (`LIKE` for an ASCII term, `suvadu_contains_ci`
/// for any other) and never drops one it has already counted.
pub fn matches_loosely(haystack: &str, loose_tokens: &[String]) -> bool {
    let hay = normalize_separators(haystack).to_lowercase();
    loose_tokens.iter().all(|t| hay.contains(&t.to_lowercase()))
}

/// Separator-insensitive matching's normalization, shared with the
/// `entries_norm_fts` index; see [`crate::util::normalize_separators`].
pub use crate::util::normalize_separators;

/// The one definition of `fuzzy` eligibility, shared with the SQL predicate
/// `suvadu_subseq_ci` so the database and the in-memory pass can never
/// disagree about which entries the mode accepts.
pub use crate::util::is_subsequence_ci;

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
    fn case_folding_per_mode_is_as_documented() {
        assert!(MatchMode::Literal.matches("cat README.md", "readme"));
        assert!(MatchMode::Literal.matches("cat readme.md", "README"));
        // Non-ASCII is compared case-sensitively — documented, not accidental.
        assert!(!MatchMode::Literal.matches("echo écho", "ÉCHO"));
        assert!(MatchMode::Literal.matches("echo écho", "écho"));
        // fuzzy is the database's own subsequence rule, which folds Unicode.
        assert!(MatchMode::Fuzzy.matches("echo écho", "ÉCHO"));
        assert!(MatchMode::Fuzzy.matches("echo Émile", "eém"));
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
    fn fuzzy_is_decided_by_the_subsequence_rule_alone() {
        // No per-character substring clauses: a one-letter LIKE has no
        // trigram to look up, so each one scanned the whole index only to
        // admit a superset the subsequence predicate then had to test anyway.
        for query in ["gco", "kubectl", "a a  B"] {
            let plan = MatchMode::Fuzzy.plan(query);
            assert!(plan.tokens.is_empty(), "{query:?}: {:?}", plan.tokens);
            assert_eq!(plan.query, None, "{query:?}");
            assert!(plan.allow_subsequence);
            assert!(plan.rerank);
        }
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

    fn loose(query: &str) -> Option<Vec<String>> {
        MatchMode::Terms.loose_plan(query).map(|p| p.loose_tokens)
    }

    #[test]
    fn the_terms_fallback_normalizes_each_term_like_the_commands() {
        assert_eq!(loose("hiveagent"), Some(vec!["hiveagent".to_string()]));
        assert_eq!(loose(" Hive-Agent "), Some(vec!["hiveagent".to_string()]));
        assert_eq!(
            loose("git  push-force x_y.z/w:v"),
            Some(vec![
                "git".to_string(),
                "pushforce".to_string(),
                "xyzwv".to_string()
            ])
        );
        // A term that was only separators narrows nothing and is dropped…
        assert_eq!(loose("hive -- agent"), loose("hive agent"));
        // …and a query of nothing else has no fallback at all.
        assert_eq!(loose("-- . _"), None);
        assert_eq!(loose("   "), None);
    }

    #[test]
    fn the_terms_fallback_is_one_rule_and_still_ranked() {
        let plan = MatchMode::Terms.loose_plan("hive-agent start").unwrap();
        assert!(plan.tokens.is_empty(), "no strict clause rides along");
        assert_eq!(plan.query, None);
        assert!(!plan.prefix);
        assert!(!plan.allow_subsequence);
        assert!(plan.rerank);
        // The strict plan never carries loose clauses.
        assert!(MatchMode::Terms.plan("hive-agent").loose_tokens.is_empty());
    }

    #[test]
    fn only_terms_has_a_separator_insensitive_fallback() {
        for mode in [MatchMode::Literal, MatchMode::Prefix, MatchMode::Fuzzy] {
            assert_eq!(mode.loose_plan("hiveagent"), None, "{mode:?}");
        }
    }

    #[test]
    fn a_loose_match_ignores_separators_in_the_command() {
        let terms = loose("hiveagent").unwrap();
        for command in [
            "ssh hive-agent",
            "hive_agent run",
            "hive.agent",
            "cd hive/agent",
            "hive:agent",
            "HIVE-AGENT",
        ] {
            assert!(matches_loosely(command, &terms), "{command:?}");
        }
        assert!(!matches_loosely(
            "hive agentx",
            &loose("hiveagentz").unwrap()
        ));
        // Every term must appear, as in strict `terms`.
        let two = loose("hive-agent restart").unwrap();
        assert!(matches_loosely("hive_agent --restart", &two));
        assert!(!matches_loosely("hive_agent --stop", &two));
        // The strict rule is unchanged: punctuation still is not stripped.
        assert!(!MatchMode::Terms.matches("ssh hive-agent", "hiveagent"));
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
