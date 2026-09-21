use crate::models::{Entry, SearchField};
use crate::repository::{QueryFilter, Repository, SessionScoped};

use super::{MatchMode, RecallScope, SearchAction, SearchApp};

/// Textual match-quality tier used as the primary search-ranking key, so a
/// closer textual match always beats a looser one regardless of cwd/recency
/// boosts. `field_lc`/`query_lc` are lower-cased; `atoms` is the query split on
/// whitespace. Higher is better:
///   4 = `query` is a prefix of the field
///   3 = `query` is a contiguous substring
///   2 = every atom appears as a literal substring, in query order
///   1 = every atom appears as a literal substring (any order)
///   0 = matched only as a fuzzy subsequence (no literal atom) — e.g. an
///       abbreviation like "gco" → "git checkout"
pub(super) fn match_tier(field_lc: &str, query_lc: &str, atoms: &[&str]) -> u8 {
    if query_lc.is_empty() {
        return 0;
    }
    if field_lc.starts_with(query_lc) {
        return 4;
    }
    if field_lc.contains(query_lc) {
        return 3;
    }
    if !atoms.iter().all(|a| field_lc.contains(a)) {
        return 0;
    }
    // All atoms are literal substrings — are they in query order (non-overlapping)?
    let mut pos = 0;
    for a in atoms {
        match field_lc[pos..].find(a) {
            Some(i) => pos += i + a.len(),
            None => return 1, // present, but not in order
        }
    }
    2
}

impl SearchApp {
    pub(super) fn get_selected_entry(&self) -> Option<&Entry> {
        self.table_state
            .selected()
            .and_then(|idx| self.entries.get(idx))
    }

    pub(super) fn get_selected_command(&self) -> Option<String> {
        self.get_selected_entry().map(|entry| entry.command.clone())
    }

    /// Count active filters for badge display
    pub(super) const fn active_filter_count(&self) -> usize {
        let mut count = 0;
        if self.filters.after.is_some() {
            count += 1;
        }
        if self.filters.before.is_some() {
            count += 1;
        }
        if self.filters.tag_id.is_some() {
            count += 1;
        }
        if self.filters.exit_code.is_some() {
            count += 1;
        }
        if self.filters.executor_type.is_some() {
            count += 1;
        }
        if self.filters.failed_only {
            count += 1;
        }
        if self.filters.bookmarks_only {
            count += 1;
        }
        count
    }

    /// Point `filters.cwd` at whatever the active scope means, so the query,
    /// the status row and the filter badges can never disagree about it.
    ///
    /// The directory values come from the already-resolved
    /// [`RecallContext`](super::RecallContext) rather than from the
    /// filesystem, so a scope keeps meaning the same thing for as long as the
    /// user is looking at it.
    pub(super) fn sync_scope_filters(&mut self) {
        self.filters.cwd = match self.recall.scope {
            RecallScope::Directory => self.recall.context.cwd.clone(),
            RecallScope::Workspace => self.recall.context.workspace_root(),
            RecallScope::All | RecallScope::Session => None,
        };
    }

    /// Switch to `scope`, keeping the derived filters in step.
    pub(super) fn set_scope(&mut self, scope: RecallScope) {
        self.recall.scope = scope;
        self.recall.scope_note = None;
        self.sync_scope_filters();
        self.pagination.page = 1;
    }

    /// Move to the next scope that is actually usable here (`^P`).
    pub(super) fn cycle_scope(&mut self) -> SearchAction {
        let next = self.recall.context.next_available_scope(self.recall.scope);
        self.set_scope(next);
        self.status_message = Some((
            format!("Scope: {}", next.status_value()),
            std::time::Instant::now(),
        ));
        SearchAction::Reload
    }

    /// Move to the next matching mode (`^X`).
    pub(super) fn cycle_match_mode(&mut self) -> SearchAction {
        let next = self.recall.match_mode.next();
        self.recall.match_mode = next;
        self.pagination.page = 1;
        self.status_message = Some((
            format!("Match: {} ({})", next.label(), next.describe()),
            std::time::Instant::now(),
        ));
        SearchAction::Reload
    }

    /// One action back to all of history (`^R`).
    ///
    /// Clears every narrowing the user can have accumulated and returns the
    /// matching mode to the default. Agent visibility is deliberately *not*
    /// touched: a reset must never quietly pull excluded agent commands into
    /// view. The no-results state names `^A` separately for that.
    pub(super) fn reset_to_all_history(&mut self) -> SearchAction {
        self.recall.match_mode = MatchMode::default();
        self.filters.after = None;
        self.filters.before = None;
        self.filters.tag_id = None;
        self.filters.exit_code = None;
        self.filters.executor_type = None;
        self.filters.executor_sel = 0;
        self.filters.failed_only = false;
        self.filters.bookmarks_only = false;
        self.set_scope(RecallScope::All);
        self.status_message = Some(("Reset to all history".into(), std::time::Instant::now()));
        SearchAction::Reload
    }

    /// Build the entry query for the current state.
    ///
    /// `query`/`prefix`/`tokens` come from [`MatchMode::plan`]: the matching
    /// mode decides *what* SQL is asked for, the scope decides *where* it
    /// looks, and the two never interfere.
    fn build_query_filter<'a>(
        &'a self,
        query: Option<&'a str>,
        tokens: &'a [String],
        prefix: bool,
    ) -> SessionScoped<'a> {
        SessionScoped {
            filter: QueryFilter {
                query_tokens: tokens,
                after: self.filters.after,
                before: self.filters.before,
                tag_id: self.filters.tag_id,
                exit_code: self.filters.exit_code,
                query,
                prefix_match: prefix,
                executor: self.filters.executor_type.as_deref(),
                cwd: self.filters.cwd.as_deref(),
                field: self.view.search_field,
                exclude_agents: !self.filters.show_agents,
                // "Workspace" means the whole project tree, not just its root
                // directory; every other scope matches the directory exactly.
                cwd_prefix: self.recall.scope == RecallScope::Workspace,
                failed_only: self.filters.failed_only,
                bookmarked_only: self.filters.bookmarks_only,
                exclude_dirs: &[],
            },
            session_id: if self.recall.scope == RecallScope::Session {
                self.recall.context.session_id.as_deref()
            } else {
                None
            },
        }
    }

    /// The query object for the current state, including the `fuzzy` mode's
    /// subsequence rule, so SQL decides eligibility completely: what
    /// `count_filtered` counts is exactly what `LIMIT`/`OFFSET` can walk.
    fn build_matched_query<'a>(
        &'a self,
        plan: &'a super::matching::QueryPlan,
        subsequence: Option<&'a str>,
    ) -> crate::repository::Subsequence<'a, SessionScoped<'a>> {
        crate::repository::Subsequence {
            inner: self.build_query_filter(plan.query.as_deref(), &plan.tokens, plan.prefix),
            needle: subsequence,
            field: self.view.search_field,
        }
    }

    /// The trimmed query when the mode matches by subsequence, else `None`.
    fn subsequence_needle<'a>(&'a self, plan: &super::matching::QueryPlan) -> Option<&'a str> {
        plan.allow_subsequence
            .then(|| self.query.trim())
            .filter(|q| !q.is_empty())
    }

    /// Rank with the default (`terms`) rule: a pure subsequence is not a match.
    #[cfg(test)]
    pub(super) fn fuzzy_score(
        entries: Vec<Entry>,
        query: &str,
        boost_cwd: Option<&str>,
        field: SearchField,
        length_threshold: usize,
        human_boost_percent: u32,
        cwd_boost_percent: u32,
    ) -> Vec<Entry> {
        Self::fuzzy_score_mode(
            entries,
            query,
            boost_cwd,
            field,
            length_threshold,
            human_boost_percent,
            cwd_boost_percent,
            false,
        )
    }

    /// As `fuzzy_score`, but `allow_subsequence` selects the `fuzzy` mode's
    /// eligibility rule (`gco` → `git checkout`) instead of the default one.
    /// Only `MatchMode::Fuzzy` passes `true`; that single flag is the whole
    /// behavioural difference between the two modes.
    ///
    /// Eligibility is decided *before* and independently of the ranking tier,
    /// and the relevance score never removes an entry — it only orders what
    /// the mode already accepted. That is what lets the database count the
    /// same set this function keeps.
    #[allow(
        clippy::cast_precision_loss,
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        clippy::too_many_arguments
    )]
    pub(super) fn fuzzy_score_mode(
        entries: Vec<Entry>,
        query: &str,
        boost_cwd: Option<&str>,
        field: SearchField,
        length_threshold: usize,
        human_boost_percent: u32,
        cwd_boost_percent: u32,
        allow_subsequence: bool,
    ) -> Vec<Entry> {
        use nucleo_matcher::pattern::{CaseMatching, Normalization, Pattern};
        use nucleo_matcher::{Config as MatcherConfig, Matcher, Utf32Str};

        let threshold = (length_threshold.max(1)) as f64;

        let mut matcher = Matcher::new(MatcherConfig::DEFAULT);
        let pattern = Pattern::parse(query, CaseMatching::Smart, Normalization::Smart);

        // Lower-cased, trimmed query for the match-quality tier (below). nucleo
        // splits a multi-word query into independent atoms, so it ranks a
        // scattered match ("git ... add") the same as a contiguous one
        // ("git add"). We restore that phrase/word-order signal as the primary
        // sort key.
        let query_lc = query.trim().to_lowercase();
        let atoms: Vec<&str> = query_lc.split_whitespace().collect();

        // (entry, match-quality tier, boosted fuzzy score)
        let mut scored: Vec<(Entry, u8, u32)> = Vec::new();
        let mut buf = Vec::new();

        for entry in entries {
            buf.clear();
            // One definition of "the text this field searches", shared with
            // the SQL side (see Entry::search_field_text) so the database and
            // the scorer can never disagree about what matched.
            let field_text = entry.search_field_text(field);
            let field_value: &str = field_text.as_ref();
            // ── Matching: is this entry eligible at all? ───────────────
            // Decided by the mode's own rule, never by how well it ranks.
            //
            // `terms` (and every non-subsequence mode) requires each typed
            // token to appear as a literal substring, which is tier >= 1:
            // pure subsequence hits surface unrelated commands, e.g.
            // "git add" matching "git rev-parse". `fuzzy` asks the documented
            // whole-query subsequence rule instead — *instead*, not as a
            // fallback for tier 0, or a query whose words each appear would
            // slip past the ordering the mode's own help promises.
            let tier = match_tier(&field_value.to_lowercase(), &query_lc, &atoms);
            let eligible = if allow_subsequence {
                MatchMode::Fuzzy.matches(field_value, query)
            } else {
                tier >= 1
            };
            if !eligible {
                continue;
            }

            // ── Ranking: order what matching already accepted. ─────────
            // A missing nucleo score means "no relevance signal", not "not a
            // match" — nucleo's smart-case rule is stricter than the
            // documented, case-insensitive one — so it scores 0 and keeps its
            // place rather than disappearing from a result set the database
            // has already counted.
            let haystack = Utf32Str::new(field_value, &mut buf);
            let score = pattern.score(haystack, &mut matcher).unwrap_or(0);

            // Penalise long commands — short matches are more relevant.
            // Commands ≤ length_threshold chars keep full score; longer
            // ones are scaled down by sqrt(threshold/len).
            let cmd_len = field_value.len().max(1) as f64;
            let length_factor = if cmd_len <= threshold {
                1.0
            } else {
                (threshold / cmd_len).sqrt()
            };
            let mut final_score = (f64::from(score) * length_factor) as u32;

            // Boost human-executed commands over agent commands
            if entry.is_human() && human_boost_percent > 0 {
                final_score = final_score.saturating_add(
                    (f64::from(final_score) * f64::from(human_boost_percent) / 100.0) as u32,
                );
            }
            // Boost same-CWD commands
            if boost_cwd.is_some_and(|cwd| entry.cwd == cwd) && cwd_boost_percent > 0 {
                final_score = final_score.saturating_add(
                    (f64::from(final_score) * f64::from(cwd_boost_percent) / 100.0) as u32,
                );
            }

            // Match-quality tier (see `match_tier`): textual match quality
            // dominates the cwd/recency boosts, which only break ties within
            // a tier.
            scored.push((entry, tier, final_score));
        }

        scored.sort_by(|a, b| {
            // Primary: match-quality tier (prefix > substring > scattered)
            b.1.cmp(&a.1)
                // Secondary: boosted fuzzy score (descending)
                .then_with(|| b.2.cmp(&a.2))
                // Tiebreaker: interactively-typed entries (terminal/IDE) first,
                // above agent/bot/ci/script commands.
                .then_with(|| b.0.is_interactive().cmp(&a.0.is_interactive()))
        });
        scored.into_iter().map(|(e, _, _)| e).collect()
    }

    /// Stable re-sort: combined context + human-first ranking in a single pass.
    /// Primary: same-CWD entries first (if `context_boost` enabled).
    /// Secondary: human-executed entries above agent entries.
    /// This avoids the competing-sort problem where two sequential sorts
    /// could undo each other's grouping.
    #[cfg(test)]
    pub(super) fn apply_combined_sort(entries: &mut [Entry], context_cwd: Option<&str>) {
        entries.sort_by(|a, b| {
            // Primary: local directory first (if context boost is active)
            if let Some(cwd) = context_cwd {
                let a_local = a.cwd == cwd;
                let b_local = b.cwd == cwd;
                let cwd_cmp = b_local.cmp(&a_local);
                if cwd_cmp != std::cmp::Ordering::Equal {
                    return cwd_cmp;
                }
            }
            // Secondary: human entries first
            b.is_human().cmp(&a.is_human())
        });
    }

    /// How many of the newest eligible matches are ranked by relevance.
    ///
    /// **Ranking versus pagination.** Matching and counting are complete: SQL
    /// decides eligibility for every mode (including `fuzzy`, via
    /// `suvadu_subseq_ci`), so `total_items` is the true number of matches in
    /// the whole history and every page it implies can be fetched. Relevance
    /// ranking, which needs the candidates in memory, is deliberately *not*
    /// complete: only the newest `RANK_WINDOW` matches are ranked. Anything
    /// beyond that window is paged straight from the database in recency
    /// order — the order those pages would have had anyway, since a ranking
    /// that could only see part of the result set would be arbitrary there.
    ///
    /// The consequence to hold on to: a partial ranking window never becomes
    /// the whole result set, and a count is never reported that cannot be
    /// paged to.
    const RANK_WINDOW: usize = 5_000;

    /// The ranking window rounded up to whole pages, so no single page ever
    /// straddles the boundary between ranked results and the recency tail.
    fn rank_window_size(&self) -> usize {
        let page_size = self.pagination.page_size.max(1);
        page_size * Self::RANK_WINDOW.div_ceil(page_size)
    }

    /// Re-run the query: count every eligible match, rank the newest window
    /// of them, and show the first page.
    ///
    /// This is the one pipeline. Startup (`--query`), typing, editing, mode
    /// and scope changes and pagination all arrive here or at
    /// [`Self::set_page`], so they cannot disagree about what matches.
    pub(super) fn reload_entries(
        &mut self,
        repo: &Repository,
    ) -> Result<(), Box<dyn std::error::Error>> {
        // The matching mode decides how the query becomes SQL; see
        // `MatchMode::plan`. Every mode narrows *and decides* in the database,
        // so how far back a match lives never decides whether it is found.
        let plan = self.recall.match_mode.plan(&self.query);
        let window = if plan.rerank {
            self.rank_window_size()
        } else {
            0
        };

        // Borrow-only phase: everything that reads `self` through the query.
        let (total, ranked, ranked_counts) = {
            let qf = self.build_matched_query(&plan, self.subsequence_needle(&plan));

            if window == 0 {
                let total = usize::try_from(if self.view.unique_mode {
                    repo.count_unique_filtered(&qf)?
                } else {
                    repo.count_filtered(&qf)?
                })?;
                (total, Vec::new(), std::collections::HashMap::new())
            } else {
                let boost_cwd = if self.view.context_boost {
                    self.view.current_cwd.as_deref()
                } else {
                    None
                };
                let (candidates, counts) = if self.view.unique_mode {
                    let rows = repo.get_unique_entries_filtered(window, 0, &qf, false)?;
                    let (entries, counts): (Vec<Entry>, Vec<i64>) = rows.into_iter().unzip();
                    let map = Self::count_map(&entries, &counts);
                    (entries, map)
                } else {
                    (
                        repo.get_entries_filtered(window, 0, &qf)?,
                        std::collections::HashMap::new(),
                    )
                };
                // A window that came back short *is* the whole match set, so
                // the count is already known and the second query — the
                // expensive half of a fuzzy reload — is skipped. Only a full
                // window leaves anything to count.
                let total = if candidates.len() < window {
                    candidates.len()
                } else {
                    usize::try_from(if self.view.unique_mode {
                        repo.count_unique_filtered(&qf)?
                    } else {
                        repo.count_filtered(&qf)?
                    })?
                };
                let ranked = Self::fuzzy_score_mode(
                    candidates,
                    &self.query,
                    boost_cwd,
                    self.view.search_field,
                    self.view.length_threshold,
                    self.view.human_boost_percent,
                    self.view.cwd_boost_percent,
                    plan.allow_subsequence,
                );
                (total, ranked, counts)
            }
        };

        self.pagination.total_items = total;
        self.ranked_window = ranked;
        self.unique_counts = ranked_counts;
        self.set_page(repo, 1)
    }

    /// Map entry id → occurrence count, for unique mode's badge.
    fn count_map(entries: &[Entry], counts: &[i64]) -> std::collections::HashMap<i64, i64> {
        let mut map = std::collections::HashMap::new();
        for (entry, count) in entries.iter().zip(counts.iter()) {
            if let Some(id) = entry.id {
                map.insert(id, *count);
            }
        }
        map
    }

    /// Show page `page` of the current result set.
    ///
    /// Pages inside the ranking window are served from it; the rest come
    /// straight from the database at the same offset, in recency order (see
    /// [`Self::RANK_WINDOW`]). Because the window is a whole number of pages,
    /// one page is never half ranked and half not.
    pub(super) fn set_page(
        &mut self,
        repo: &Repository,
        page: usize,
    ) -> Result<(), Box<dyn std::error::Error>> {
        self.pagination.page = page.max(1);
        let page_size = self.pagination.page_size;
        let offset = (self.pagination.page - 1) * page_size;

        if offset < self.ranked_window.len() {
            let end = (offset + page_size).min(self.ranked_window.len());
            self.entries = self.ranked_window[offset..end].to_vec();
        } else {
            // Beyond the ranked window, or a mode that never ranks: the
            // database answers directly. The plan must be the one
            // `reload_entries` used, or page 2 would answer a different
            // question from page 1.
            let plan = self.recall.match_mode.plan(&self.query);
            let (entries, counts) = {
                let qf = self.build_matched_query(&plan, self.subsequence_needle(&plan));
                if self.view.unique_mode {
                    // `sort_alphabetically` must match the ordering the window
                    // was drawn from, or a tail page could repeat or skip.
                    let rows =
                        repo.get_unique_entries_filtered(page_size, offset, &qf, !plan.rerank)?;
                    let (entries, counts): (Vec<Entry>, Vec<i64>) = rows.into_iter().unzip();
                    let map = Self::count_map(&entries, &counts);
                    (entries, map)
                } else {
                    (
                        repo.get_entries_filtered(page_size, offset, &qf)?,
                        std::collections::HashMap::new(),
                    )
                }
            };
            if self.view.unique_mode {
                self.unique_counts.extend(counts);
            }
            self.entries = entries;
        }

        self.table_state.select(if self.entries.is_empty() {
            None
        } else {
            Some(0)
        });
        Ok(())
    }
}
