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
//!      [`habit_factor`] × [`outcome_factor`] × [`sequence_factor`].
//! 3. Interactively typed rows above agent, bot, CI and script rows.
//! 4. The window's own order: newest first in Executions, most runs then
//!    newest in Commands.
//!
//! A product, rather than a cascade of keys, is what lets the signals trade
//! off: a habit beats one newer run, a command run here beats a slightly
//! more frequent one run elsewhere, a command that usually fails drops below
//! a working one it would otherwise tie with, and what you nearly always run
//! next (`git commit` after `git add -A`) beats a more frequent command that
//! has nothing to do with what you just did. Recent rank multiplies
//! by 1, so its order is exactly what it was before any of this existed.
//!
//! The factors come from every recorded run of exactly the command's text,
//! not just the runs that matched. They are read once per command per recall
//! session and kept, so typing a longer query only looks up commands it has
//! not seen yet. The sequence signal depends only on the shell session's last
//! command, so it is read once per recall session.

use std::collections::HashMap;
use std::sync::Mutex;

use crate::db::DbResult;
use crate::frecency::CommandStats;
use crate::models::Entry;
use crate::repository::{Followers, Repository};

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

/// A command counts as following another only when it started within this
/// long of the other finishing.
const SEQUENCE_WINDOW_MS: i64 = 10 * 60_000;

/// How many of the previous command's newest runs are looked at.
const SEQUENCE_SAMPLE: usize = 500;

/// A pair seen fewer times than this is a coincidence, not a habit.
const MIN_PAIRS: u32 = 2;

/// The boost for a command that followed every time.
const SEQUENCE_WEIGHT: f64 = 2.0;

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

/// `1 + 2 × share`, where `share` is how often `command` was what ran next
/// after the previous command, among its runs that something followed
/// within [`SEQUENCE_WINDOW_MS`] in the same shell session. 1 for a pair
/// seen fewer than [`MIN_PAIRS`] times, so up to 3 for a command that has
/// followed every time.
pub(super) fn sequence_factor(followers: &Followers, command: &str) -> f64 {
    let pairs = followers.next.get(command).copied().unwrap_or(0);
    if pairs < MIN_PAIRS || followers.occurrences == 0 {
        return 1.0;
    }
    let share = f64::from(pairs) / f64::from(followers.occurrences);
    SEQUENCE_WEIGHT.mul_add(share, 1.0)
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
    /// What followed the last command of a shell session, by session and
    /// whether agent runs count.
    sequences: Mutex<HashMap<(String, bool), Followers>>,
}

impl SmartRank {
    pub(super) fn new() -> Self {
        Self {
            now: chrono::Utc::now().timestamp_millis(),
            stats: Mutex::new([HashMap::new(), HashMap::new()]),
            sequences: Mutex::new(HashMap::new()),
        }
    }

    /// What followed the last command run in `session`, read once.
    fn followers(
        &self,
        repo: &Repository,
        session: &str,
        include_agents: bool,
    ) -> DbResult<Followers> {
        let key = (session.to_string(), include_agents);
        let lock = || {
            self.sequences
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
        };
        if let Some(known) = lock().get(&key) {
            return Ok(known.clone());
        }
        let followers = match repo.last_command_in_session(session)? {
            Some(previous) => repo.followers_of(
                &previous,
                SEQUENCE_WINDOW_MS,
                SEQUENCE_SAMPLE,
                include_agents,
            )?,
            None => Followers::default(),
        };
        lock().insert(key, followers.clone());
        Ok(followers)
    }

    /// The Smart-rank factor for each of `candidates`, in order. `session`
    /// is the shell session to follow on from, or `None` for no sequence
    /// boost.
    pub(super) fn factors(
        &self,
        repo: &Repository,
        candidates: &[Entry],
        include_agents: bool,
        session: Option<&str>,
    ) -> DbResult<Vec<f64>> {
        let followers = match session {
            Some(session) => self.followers(repo, session, include_agents)?,
            None => Followers::default(),
        };
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
                let history = known
                    .get(&e.command)
                    .map_or(1.0, |s| habit_factor(s) * outcome_factor(s));
                history * sequence_factor(&followers, &e.command)
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

    #[test]
    fn only_a_repeated_sequence_counts() {
        let followers = Followers {
            occurrences: 10,
            next: [("git commit".to_string(), 8), ("ls".to_string(), 1)]
                .into_iter()
                .collect(),
        };
        assert!((sequence_factor(&followers, "git commit") - 2.6).abs() < 1e-12);
        assert!((sequence_factor(&followers, "ls") - 1.0).abs() < 1e-12);
        assert!((sequence_factor(&followers, "make") - 1.0).abs() < 1e-12);
        assert!((sequence_factor(&Followers::default(), "make") - 1.0).abs() < 1e-12);
    }
}
