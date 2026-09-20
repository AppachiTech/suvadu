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

    /// As `fuzzy_score`, but `allow_subsequence` decides whether an
    /// abbreviation that shares no literal token (`gco` → `git checkout`)
    /// counts as a match. Only `MatchMode::Fuzzy` passes `true`; that single
    /// flag is the whole behavioural difference between the two modes.
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
            let executor_str;
            let field_value: &str = match field {
                SearchField::Cwd => &entry.cwd,
                SearchField::Session => &entry.session_id,
                SearchField::Executor => {
                    executor_str = entry.executor_type.as_deref().unwrap_or("").to_string();
                    &executor_str
                }
                SearchField::Command => &entry.command,
            };
            let haystack = Utf32Str::new(field_value, &mut buf);
            if let Some(score) = pattern.score(haystack, &mut matcher) {
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
                let tier = match_tier(&field_value.to_lowercase(), &query_lc, &atoms);

                // Require every typed token to actually appear as a literal
                // substring (tier >= 1). Pure subsequence matches (tier 0)
                // surface unrelated commands — e.g. "git add" matching
                // "git rev-parse", or random gibberish matching long commands
                // whose characters happen to contain it as a subsequence.
                // Results always contain what you typed; nucleo still ranks
                // within the literal matches. `fuzzy` mode opts out of this
                // guard — surfacing abbreviations is exactly what it is for,
                // but only for entries the documented subsequence rule
                // actually accepts, so the mode matches its own help.
                if tier == 0 && !(allow_subsequence && MatchMode::Fuzzy.matches(field_value, query))
                {
                    continue;
                }

                scored.push((entry, tier, final_score));
            }
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

    pub(super) fn reload_entries(
        &mut self,
        repo: &Repository,
    ) -> Result<(), Box<dyn std::error::Error>> {
        // The matching mode decides how the query becomes SQL; see
        // `MatchMode::plan`. Every mode narrows in the database, so how far
        // back a match lives never decides whether it is found.
        let plan = self.recall.match_mode.plan(&self.query);

        // `literal` and `prefix` are exactly what SQL returns, so they keep
        // the database's recency order and paginate in the database. `terms`
        // and `fuzzy` rank in memory, which is what `plan.rerank` marks.
        if plan.rerank {
            // Narrow candidates in SQL, then score + rank them.
            //
            // The scorer only keeps entries where every typed token appears as
            // a literal substring (see `match_tier`), so asking SQL for the
            // same thing yields the same set from the *whole* history. Before,
            // this fetched the newest 5,000 rows and matched in memory, which
            // silently hid any older match. The limit now bounds how many
            // matches are ranked, not how much history is searched.
            const MAX_RANKED_MATCHES: usize = 5_000;
            let qf = self.build_query_filter(None, &plan.tokens, false);

            if self.view.unique_mode {
                let unique_res =
                    repo.get_unique_entries_filtered(MAX_RANKED_MATCHES, 0, &qf, false)?;
                let (entries, counts): (Vec<Entry>, Vec<i64>) = unique_res.into_iter().unzip();

                let mut count_map = std::collections::HashMap::new();
                for (entry, count) in entries.iter().zip(counts.iter()) {
                    if let Some(id) = entry.id {
                        count_map.insert(id, *count);
                    }
                }

                let boost_cwd = if self.view.context_boost {
                    self.view.current_cwd.as_deref()
                } else {
                    None
                };
                let scored = Self::fuzzy_score_mode(
                    entries,
                    &self.query,
                    boost_cwd,
                    self.view.search_field,
                    self.view.length_threshold,
                    self.view.human_boost_percent,
                    self.view.cwd_boost_percent,
                    plan.allow_subsequence,
                );
                self.unique_counts = count_map;
                self.fuzzy_results = scored;
            } else {
                let entries = repo.get_entries_filtered(MAX_RANKED_MATCHES, 0, &qf)?;

                let boost_cwd = if self.view.context_boost {
                    self.view.current_cwd.as_deref()
                } else {
                    None
                };
                self.fuzzy_results = Self::fuzzy_score_mode(
                    entries,
                    &self.query,
                    boost_cwd,
                    self.view.search_field,
                    self.view.length_threshold,
                    self.view.human_boost_percent,
                    self.view.cwd_boost_percent,
                    plan.allow_subsequence,
                );
            }

            self.pagination.total_items = self.fuzzy_results.len();
            self.pagination.page = 1;
            let end = self.pagination.page_size.min(self.fuzzy_results.len());
            self.entries = self.fuzzy_results[..end].to_vec();
        } else {
            // Exact path: SQL does the whole match and the pagination, and
            // results stay in recency order — that predictability is the
            // point of the literal and prefix modes.
            self.fuzzy_results.clear();
            let qf = self.build_query_filter(plan.query.as_deref(), &[], plan.prefix);

            if self.view.unique_mode {
                let new_count = repo.count_unique_filtered(&qf)?;
                let unique_res =
                    repo.get_unique_entries_filtered(self.pagination.page_size, 0, &qf, true)?;
                // qf no longer needed — safe to mutate self
                self.pagination.total_items = usize::try_from(new_count)?;
                self.pagination.page = 1;
                let (entries, counts): (Vec<Entry>, Vec<i64>) = unique_res.into_iter().unzip();
                self.unique_counts.clear();
                for (entry, count) in entries.iter().zip(counts.iter()) {
                    if let Some(id) = entry.id {
                        self.unique_counts.insert(id, *count);
                    }
                }
                self.entries = entries;
            } else {
                let new_count = repo.count_filtered(&qf)?;
                let new_entries = repo.get_entries_filtered(self.pagination.page_size, 0, &qf)?;
                // qf no longer needed — safe to mutate self
                self.pagination.total_items = usize::try_from(new_count)?;
                self.pagination.page = 1;
                self.entries = new_entries;
            }
        }

        self.table_state.select(if self.entries.is_empty() {
            None
        } else {
            Some(0)
        });
        Ok(())
    }

    pub(super) fn set_page(
        &mut self,
        repo: &Repository,
        page: usize,
    ) -> Result<(), Box<dyn std::error::Error>> {
        self.pagination.page = page;
        let offset = (self.pagination.page - 1) * self.pagination.page_size;

        if self.fuzzy_results.is_empty() {
            // Standard DB-level pagination. The plan must match the one
            // `reload_entries` used, or page 2 would answer a different
            // question from page 1.
            let plan = self.recall.match_mode.plan(&self.query);
            let qf = self.build_query_filter(plan.query.as_deref(), &plan.tokens, plan.prefix);

            if self.view.unique_mode {
                let unique_res =
                    repo.get_unique_entries_filtered(self.pagination.page_size, offset, &qf, true)?;
                let (entries, counts): (Vec<Entry>, Vec<i64>) = unique_res.into_iter().unzip();
                self.unique_counts.clear();
                for (entry, count) in entries.iter().zip(counts.iter()) {
                    if let Some(id) = entry.id {
                        self.unique_counts.insert(id, *count);
                    }
                }
                self.entries = entries;
            } else {
                self.entries = repo.get_entries_filtered(self.pagination.page_size, offset, &qf)?;
            }
        } else {
            // Fuzzy mode: paginate from in-memory scored results
            let end = (offset + self.pagination.page_size).min(self.fuzzy_results.len());
            self.entries = if offset < self.fuzzy_results.len() {
                self.fuzzy_results[offset..end].to_vec()
            } else {
                Vec::new()
            };
        }

        self.table_state.select(if self.entries.is_empty() {
            None
        } else {
            Some(0)
        });
        Ok(())
    }
}
