//! What Smart rank (`^S`) knows about a command beyond its text.
//!
//! Ranking orders what matching already chose and never changes it (see
//! [`super::matching`]). Within that, the order is:
//!
//! 1. **Match tier** — whole-query prefix, contiguous substring, words in
//!    order, words in any order ([`super::data::match_tier`]); then the same
//!    tiers with separators ignored, for the fallback's rows. Nothing below
//!    can lift a row over a closer textual match.
//! 2. **One combined score**, the product of
//!    - the fuzzy score, scaled down for a long command,
//!    - the human boost (`search.human_boost_percent`) and, in Smart rank,
//!      the this-directory boost (`search.cwd_boost_percent`),
//!    - and, in Smart rank only, the factor this module computes:
//!      [`habit_factor`] × [`outcome_factor`].
//! 3. Interactively typed rows above agent, bot, CI and script rows.
//! 4. The window's own order: newest first in Executions, most runs then
//!    newest in Commands.
//!
//! A product, rather than a cascade of keys, is what lets the signals trade
//! off: a habit beats one newer run, a command run here beats a slightly
//! more frequent one run elsewhere, and a command that usually fails drops
//! below a working one it would otherwise tie with. Recent rank multiplies
//! by 1, so its order is exactly what it was before any of this existed.
//!
//! The factors come from every recorded run of exactly the command's text,
//! not just the runs that matched. They are read once per command per recall
//! session and kept, so typing a longer query only looks up commands it has
//! not seen yet.

use std::collections::HashMap;
use std::sync::Mutex;

use crate::db::DbResult;
use crate::frecency::CommandStats;
use crate::models::Entry;
use crate::repository::Repository;

/// How strongly frecency counts. A fourth root keeps a habit decisive
/// without letting it run away: one run in the last hour scores 2.0, twenty
/// runs a day or two ago 3.0, a thousand runs last week 5.6.
const HABIT_EXPONENT: f64 = 0.25;

/// Fewer runs with a known outcome than this say nothing about a command.
const MIN_OUTCOMES: u32 = 3;

/// A command is demoted only when more than this share of its runs failed.
const FAILURE_THRESHOLD: f64 = 0.5;

/// The factor for a command that has never succeeded.
const ALWAYS_FAILS: f64 = 0.25;

/// `(1 + frecency)^¼` — 1 for a command with no runs to count.
pub(super) fn habit_factor(stats: &CommandStats) -> f64 {
    (1.0 + stats.frecency.max(0.0)).powf(HABIT_EXPONENT)
}

/// 1 unless the command usually fails: from 1 at half its runs failing down
/// to [`ALWAYS_FAILS`] when every run did, once [`MIN_OUTCOMES`] runs have
/// a known outcome. One failure among many successes changes nothing.
pub(super) fn outcome_factor(stats: &CommandStats) -> f64 {
    if stats.outcomes < MIN_OUTCOMES {
        return 1.0;
    }
    let ratio = f64::from(stats.failures) / f64::from(stats.outcomes);
    if ratio <= FAILURE_THRESHOLD {
        return 1.0;
    }
    let over = (ratio - FAILURE_THRESHOLD) / (1.0 - FAILURE_THRESHOLD);
    (1.0 - ALWAYS_FAILS).mul_add(-over, 1.0)
}

/// Smart rank's memory for one recall session, shared by every reload in
/// it, on whichever thread runs them.
#[derive(Debug)]
pub(super) struct SmartRank {
    /// The moment frecency is measured from: fixed for the session, so a
    /// remembered score and a fresh one are always comparable.
    now: i64,
    /// Command statistics already read, with agent runs left out (`[0]`)
    /// and counted (`[1]`).
    stats: Mutex<[HashMap<String, CommandStats>; 2]>,
}

impl SmartRank {
    pub(super) fn new() -> Self {
        Self {
            now: chrono::Utc::now().timestamp_millis(),
            stats: Mutex::new([HashMap::new(), HashMap::new()]),
        }
    }

    /// The Smart-rank factor for each of `candidates`, in order.
    pub(super) fn factors(
        &self,
        repo: &Repository,
        candidates: &[Entry],
        include_agents: bool,
    ) -> DbResult<Vec<f64>> {
        let lock = || {
            self.stats
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
        };
        let slot = usize::from(include_agents);

        // What has not been read yet, without holding the lock across the
        // query.
        let mut missing: Vec<&str> = {
            let cache = lock();
            candidates
                .iter()
                .map(|e| e.command.as_str())
                .filter(|c| !cache[slot].contains_key(*c))
                .collect()
        };
        missing.sort_unstable();
        missing.dedup();
        let mut found = repo.command_stats(&missing, self.now, include_agents)?;

        let mut cache = lock();
        let known = &mut cache[slot];
        for command in missing {
            let stats = found.remove(command).unwrap_or_default();
            known.insert(command.to_string(), stats);
        }
        let factors = candidates
            .iter()
            .map(|e| {
                known
                    .get(&e.command)
                    .map_or(1.0, |s| habit_factor(s) * outcome_factor(s))
            })
            .collect();
        drop(cache);
        Ok(factors)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stats(frecency: f64, outcomes: u32, failures: u32) -> CommandStats {
        CommandStats {
            frecency,
            outcomes,
            failures,
        }
    }

    #[test]
    fn habit_grows_with_frecency_but_slowly() {
        assert!((habit_factor(&stats(0.0, 0, 0)) - 1.0).abs() < 1e-12);
        assert!((habit_factor(&stats(15.0, 0, 0)) - 2.0).abs() < 1e-12);
        assert!(habit_factor(&stats(80.0, 0, 0)) > habit_factor(&stats(16.0, 0, 0)));
        assert!(habit_factor(&stats(10_000.0, 0, 0)) < 11.0);
    }

    #[test]
    fn only_a_command_that_usually_fails_is_demoted() {
        let one = |s: CommandStats| (outcome_factor(&s) - 1.0).abs() < 1e-12;
        assert!(one(stats(1.0, 20, 1)), "one failure in twenty");
        assert!(one(stats(1.0, 10, 5)), "half");
        assert!(one(stats(1.0, 2, 2)), "too few runs to judge");
        assert!((outcome_factor(&stats(1.0, 4, 4)) - ALWAYS_FAILS).abs() < 1e-12);
        let mostly = outcome_factor(&stats(1.0, 5, 4));
        assert!(ALWAYS_FAILS < mostly && mostly < 1.0, "{mostly}");
    }
}
