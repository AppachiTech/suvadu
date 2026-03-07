use crate::models::Entry;
use crate::repository::{QueryFilter, Repository};

use super::SearchApp;

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
        if self.filter_after.is_some() {
            count += 1;
        }
        if self.filter_before.is_some() {
            count += 1;
        }
        if self.filter_tag_id.is_some() {
            count += 1;
        }
        if self.filter_exit_code.is_some() {
            count += 1;
        }
        if self.filter_executor_type.is_some() {
            count += 1;
        }
        count
    }

    /// Build a `QueryFilter` from the current search state.
    fn build_query_filter<'a>(&'a self, query: Option<&'a str>) -> QueryFilter<'a> {
        QueryFilter {
            after: self.filter_after,
            before: self.filter_before,
            tag_id: self.filter_tag_id,
            exit_code: self.filter_exit_code,
            query,
            prefix_match: false,
            executor: self.filter_executor_type.as_deref(),
            cwd: self.filter_cwd.as_deref(),
            field: &self.search_field,
        }
    }

    #[allow(
        clippy::cast_precision_loss,
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss
    )]
    pub(super) fn fuzzy_score(
        entries: Vec<Entry>,
        query: &str,
        boost_cwd: Option<&str>,
        field: &str,
    ) -> Vec<Entry> {
        use nucleo_matcher::pattern::{CaseMatching, Normalization, Pattern};
        use nucleo_matcher::{Config as MatcherConfig, Matcher, Utf32Str};

        // Scoring constants:
        // LENGTH_THRESHOLD: commands up to this length keep full score.
        // HUMAN_BOOST_FRACTION: human commands get +33% score (1/3) to
        //   surface interactive history over agent-generated commands.
        // CWD_BOOST_FRACTION: same-directory commands get +50% score (1/2)
        //   because working-directory locality is a strong relevance signal.
        const LENGTH_THRESHOLD: f64 = 80.0;
        const HUMAN_BOOST_FRACTION: u32 = 3;
        const CWD_BOOST_FRACTION: u32 = 2;

        let mut matcher = Matcher::new(MatcherConfig::DEFAULT);
        let pattern = Pattern::parse(query, CaseMatching::Smart, Normalization::Smart);

        let mut scored: Vec<(Entry, u32)> = Vec::new();
        let mut buf = Vec::new();

        for entry in entries {
            buf.clear();
            let executor_str;
            let field_value: &str = match field {
                "cwd" => &entry.cwd,
                "session" => &entry.session_id,
                "executor" => {
                    executor_str = entry.executor_type.as_deref().unwrap_or("").to_string();
                    &executor_str
                }
                _ => &entry.command,
            };
            let haystack = Utf32Str::new(field_value, &mut buf);
            if let Some(score) = pattern.score(haystack, &mut matcher) {
                // Penalise long commands — short matches are more relevant.
                // Commands ≤ LENGTH_THRESHOLD chars keep full score; longer
                // ones are scaled down by sqrt(threshold/len) so a 500-char
                // command gets ~40% score.
                let cmd_len = field_value.len().max(1) as f64;
                let length_factor = if cmd_len <= LENGTH_THRESHOLD {
                    1.0
                } else {
                    (LENGTH_THRESHOLD / cmd_len).sqrt()
                };
                let mut final_score = (f64::from(score) * length_factor) as u32;

                // Boost human-executed commands over agent commands
                if entry.is_human() {
                    final_score = final_score.saturating_add(final_score / HUMAN_BOOST_FRACTION);
                }
                // Boost same-CWD commands
                if boost_cwd.is_some_and(|cwd| entry.cwd == cwd) {
                    final_score = final_score.saturating_add(final_score / CWD_BOOST_FRACTION);
                }
                scored.push((entry, final_score));
            }
        }

        scored.sort_by(|a, b| {
            // Primary: fuzzy score (descending)
            let score_cmp = b.1.cmp(&a.1);
            if score_cmp != std::cmp::Ordering::Equal {
                return score_cmp;
            }
            // Tiebreaker: human entries first
            b.0.is_human().cmp(&a.0.is_human())
        });
        scored.into_iter().map(|(e, _)| e).collect()
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
        let use_fuzzy = self.query.len() >= 2;

        if use_fuzzy {
            // Fuzzy path: fetch broad candidates from DB, then score + rank
            const MAX_FUZZY_CANDIDATES: usize = 5_000;
            let qf = self.build_query_filter(None); // No SQL query — nucleo handles matching

            if self.unique_mode {
                let unique_res =
                    repo.get_unique_entries_filtered(MAX_FUZZY_CANDIDATES, 0, &qf, false)?;
                let (entries, counts): (Vec<Entry>, Vec<i64>) = unique_res.into_iter().unzip();

                let mut count_map = std::collections::HashMap::new();
                for (entry, count) in entries.iter().zip(counts.iter()) {
                    if let Some(id) = entry.id {
                        count_map.insert(id, *count);
                    }
                }

                let boost_cwd = if self.context_boost {
                    self.current_cwd.as_deref()
                } else {
                    None
                };
                let scored = Self::fuzzy_score(entries, &self.query, boost_cwd, &self.search_field);
                self.unique_counts = count_map;
                self.fuzzy_results = scored;
            } else {
                let entries = repo.get_entries_filtered(MAX_FUZZY_CANDIDATES, 0, &qf)?;

                let boost_cwd = if self.context_boost {
                    self.current_cwd.as_deref()
                } else {
                    None
                };
                self.fuzzy_results =
                    Self::fuzzy_score(entries, &self.query, boost_cwd, &self.search_field);
            }

            self.total_items = self.fuzzy_results.len();
            self.page = 1;
            let end = self.page_size.min(self.fuzzy_results.len());
            self.entries = self.fuzzy_results[..end].to_vec();
        } else {
            // Non-fuzzy path: use DB-level LIKE filtering + pagination
            self.fuzzy_results.clear();
            let query_param = if self.query.is_empty() {
                None
            } else {
                Some(self.query.as_str())
            };
            let qf = self.build_query_filter(query_param);

            if self.unique_mode {
                let new_count = repo.count_unique_filtered(&qf)?;
                let unique_res = repo.get_unique_entries_filtered(self.page_size, 0, &qf, true)?;
                // qf no longer needed — safe to mutate self
                self.total_items = usize::try_from(new_count)?;
                self.page = 1;
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
                let new_entries = repo.get_entries_filtered(self.page_size, 0, &qf)?;
                // qf no longer needed — safe to mutate self
                self.total_items = usize::try_from(new_count)?;
                self.page = 1;
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
        self.page = page;
        let offset = (self.page - 1) * self.page_size;

        if self.fuzzy_results.is_empty() {
            // Standard DB-level pagination
            let query_param = if self.query.is_empty() {
                None
            } else {
                Some(self.query.as_str())
            };
            let qf = self.build_query_filter(query_param);

            if self.unique_mode {
                let unique_res =
                    repo.get_unique_entries_filtered(self.page_size, offset, &qf, true)?;
                let (entries, counts): (Vec<Entry>, Vec<i64>) = unique_res.into_iter().unzip();
                self.unique_counts.clear();
                for (entry, count) in entries.iter().zip(counts.iter()) {
                    if let Some(id) = entry.id {
                        self.unique_counts.insert(id, *count);
                    }
                }
                self.entries = entries;
            } else {
                self.entries = repo.get_entries_filtered(self.page_size, offset, &qf)?;
            }
        } else {
            // Fuzzy mode: paginate from in-memory scored results
            let end = (offset + self.page_size).min(self.fuzzy_results.len());
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
