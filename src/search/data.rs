use crate::models::Entry;
use crate::repository::Repository;

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

    #[allow(
        clippy::cast_precision_loss,
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss
    )]
    pub(super) fn fuzzy_score(
        entries: Vec<Entry>,
        query: &str,
        boost_cwd: Option<&str>,
    ) -> Vec<Entry> {
        use nucleo_matcher::pattern::{CaseMatching, Normalization, Pattern};
        use nucleo_matcher::{Config as MatcherConfig, Matcher, Utf32Str};

        let mut matcher = Matcher::new(MatcherConfig::DEFAULT);
        let pattern = Pattern::parse(query, CaseMatching::Smart, Normalization::Smart);

        let mut scored: Vec<(Entry, u32)> = Vec::new();
        let mut buf = Vec::new();

        for entry in entries {
            buf.clear();
            let haystack = Utf32Str::new(&entry.command, &mut buf);
            if let Some(score) = pattern.score(haystack, &mut matcher) {
                // Penalise long commands — short matches are more relevant.
                // Commands ≤80 chars keep full score; longer ones are scaled
                // down by sqrt(80/len) so a 500-char command gets ~40% score.
                let cmd_len = entry.command.len().max(1) as f64;
                let length_factor = if cmd_len <= 80.0 {
                    1.0
                } else {
                    (80.0 / cmd_len).sqrt()
                };
                let mut final_score = (f64::from(score) * length_factor) as u32;

                // Boost human-executed commands over agent commands
                let is_human = entry.executor_type.as_deref().unwrap_or("human") == "human";
                if is_human {
                    final_score = final_score.saturating_add(final_score / 3);
                }
                // Boost same-CWD commands
                if boost_cwd.is_some_and(|cwd| entry.cwd == cwd) {
                    final_score = final_score.saturating_add(final_score / 2);
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
            let a_human = a.0.executor_type.as_deref().unwrap_or("human") == "human";
            let b_human = b.0.executor_type.as_deref().unwrap_or("human") == "human";
            b_human.cmp(&a_human)
        });
        scored.into_iter().map(|(e, _)| e).collect()
    }

    /// Stable re-sort: float same-CWD entries to top, preserving recency within each group.
    fn apply_context_sort(entries: &mut [Entry], current_cwd: &str) {
        entries.sort_by(|a, b| {
            let a_local = a.cwd == current_cwd;
            let b_local = b.cwd == current_cwd;
            b_local.cmp(&a_local) // true > false → locals first
        });
    }

    /// Stable re-sort: float human-executed entries above agent entries.
    fn apply_human_first_sort(entries: &mut [Entry]) {
        entries.sort_by(|a, b| {
            let a_human = a.executor_type.as_deref().unwrap_or("human") == "human";
            let b_human = b.executor_type.as_deref().unwrap_or("human") == "human";
            b_human.cmp(&a_human) // true > false → humans first
        });
    }

    #[allow(clippy::too_many_lines)]
    pub(super) fn reload_entries(
        &mut self,
        repo: &Repository,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let use_fuzzy = self.query.len() >= 2;

        if use_fuzzy {
            // Fuzzy path: fetch broad candidates from DB, then score + rank
            const MAX_FUZZY_CANDIDATES: usize = 10_000;

            if self.unique_mode {
                let unique_res = repo.get_unique_entries(
                    MAX_FUZZY_CANDIDATES,
                    0,
                    self.filter_after,
                    self.filter_before,
                    self.filter_tag_id,
                    self.filter_exit_code,
                    None, // No SQL query filter — nucleo handles matching
                    false,
                    false, // Recency sort (will be re-sorted by score)
                    self.filter_executor_type.as_deref(),
                    self.filter_cwd.as_deref(),
                )?;
                let (entries, counts): (Vec<Entry>, Vec<i64>) = unique_res.into_iter().unzip();

                // Build count map before fuzzy filtering
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
                let scored = Self::fuzzy_score(entries, &self.query, boost_cwd);
                self.unique_counts = count_map;
                self.fuzzy_results = scored;
            } else {
                let entries = repo.get_entries(
                    MAX_FUZZY_CANDIDATES,
                    0,
                    self.filter_after,
                    self.filter_before,
                    self.filter_tag_id,
                    self.filter_exit_code,
                    None, // No SQL query filter
                    false,
                    self.filter_executor_type.as_deref(),
                    self.filter_cwd.as_deref(),
                )?;

                let boost_cwd = if self.context_boost {
                    self.current_cwd.as_deref()
                } else {
                    None
                };
                self.fuzzy_results = Self::fuzzy_score(entries, &self.query, boost_cwd);
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

            if self.unique_mode {
                let new_count = usize::try_from(repo.count_unique_entries(
                    self.filter_after,
                    self.filter_before,
                    self.filter_tag_id,
                    self.filter_exit_code,
                    query_param,
                    false,
                    self.filter_executor_type.as_deref(),
                    self.filter_cwd.as_deref(),
                )?)?;
                self.total_items = new_count;
                self.page = 1;

                let unique_res = repo.get_unique_entries(
                    self.page_size,
                    0,
                    self.filter_after,
                    self.filter_before,
                    self.filter_tag_id,
                    self.filter_exit_code,
                    query_param,
                    false,
                    true, // Alphabetical for unique
                    self.filter_executor_type.as_deref(),
                    self.filter_cwd.as_deref(),
                )?;
                let (entries, counts): (Vec<Entry>, Vec<i64>) = unique_res.into_iter().unzip();
                self.unique_counts.clear();
                for (entry, count) in entries.iter().zip(counts.iter()) {
                    if let Some(id) = entry.id {
                        self.unique_counts.insert(id, *count);
                    }
                }
                self.entries = entries;
            } else {
                let new_count = usize::try_from(repo.count_filtered_entries(
                    self.filter_after,
                    self.filter_before,
                    self.filter_tag_id,
                    self.filter_exit_code,
                    query_param,
                    false,
                    self.filter_executor_type.as_deref(),
                    self.filter_cwd.as_deref(),
                )?)?;
                self.total_items = new_count;
                self.page = 1;

                let new_entries = repo.get_entries(
                    self.page_size,
                    0,
                    self.filter_after,
                    self.filter_before,
                    self.filter_tag_id,
                    self.filter_exit_code,
                    query_param,
                    false,
                    self.filter_executor_type.as_deref(),
                    self.filter_cwd.as_deref(),
                )?;
                self.entries = new_entries;
            }

            // Apply human-first sort, then context sort for non-fuzzy results
            Self::apply_human_first_sort(&mut self.entries);
            if self.context_boost {
                if let Some(ref cwd) = self.current_cwd {
                    Self::apply_context_sort(&mut self.entries, cwd);
                }
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

            if self.unique_mode {
                let unique_res = repo.get_unique_entries(
                    self.page_size,
                    offset,
                    self.filter_after,
                    self.filter_before,
                    self.filter_tag_id,
                    self.filter_exit_code,
                    query_param,
                    false,
                    true, // Alphabetical for unique
                    self.filter_executor_type.as_deref(),
                    self.filter_cwd.as_deref(),
                )?;
                let (entries, counts): (Vec<Entry>, Vec<i64>) = unique_res.into_iter().unzip();
                self.unique_counts.clear();
                for (entry, count) in entries.iter().zip(counts.iter()) {
                    if let Some(id) = entry.id {
                        self.unique_counts.insert(id, *count);
                    }
                }
                self.entries = entries;
            } else {
                let new_entries = repo.get_entries(
                    self.page_size,
                    offset,
                    self.filter_after,
                    self.filter_before,
                    self.filter_tag_id,
                    self.filter_exit_code,
                    query_param,
                    false,
                    self.filter_executor_type.as_deref(),
                    self.filter_cwd.as_deref(),
                )?;
                self.entries = new_entries;
            }

            // Apply human-first sort, then context sort for non-fuzzy pages
            Self::apply_human_first_sort(&mut self.entries);
            if self.context_boost {
                if let Some(ref cwd) = self.current_cwd {
                    Self::apply_context_sort(&mut self.entries, cwd);
                }
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
