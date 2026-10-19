//! Frecency: how often *and* how recently a command was run, as one number.
//!
//! Every run of a command adds a weight that depends only on how long ago it
//! started, and a command's frecency is the sum over its runs. One run in the
//! last hour is worth sixteen runs from last week, so a habit you have kept up
//! today outweighs one you dropped, while a command you have run hundreds of
//! times keeps a lead over something you ran once.
//!
//! This is the single definition of frecency in Suvadu. The MCP
//! `suggest_next` tool sums [`run_weight`] over recent runs, and recall's
//! Smart rank reads the same sum from the database through [`sql_weight`],
//! which is generated from the same table, so the two cannot drift apart.

const HOUR_MS: i64 = 3_600_000;
const DAY_MS: i64 = 24 * HOUR_MS;

/// `(younger than, weight)`, youngest first. A run at least as old as the
/// last bound weighs [`OLDER_WEIGHT`].
const AGE_WEIGHTS: &[(i64, f64)] = &[
    (HOUR_MS, 16.0),
    (DAY_MS, 8.0),
    (3 * DAY_MS, 4.0),
    (7 * DAY_MS, 1.0),
    (30 * DAY_MS, 0.5),
];

/// The weight of a run more than a month old.
const OLDER_WEIGHT: f64 = 0.25;

/// What one run that started `age_ms` ago adds to its command's frecency.
/// A start in the future (a clock that moved back) counts as just now.
pub fn run_weight(age_ms: i64) -> f64 {
    AGE_WEIGHTS
        .iter()
        .find(|(bound, _)| age_ms < *bound)
        .map_or(OLDER_WEIGHT, |(_, weight)| *weight)
}

/// [`run_weight`] as a SQL expression over the `started_at` column `column`
/// and a bound "now" parameter `now` (for example `?1`). Only constants from
/// the table above are interpolated, never user input.
pub fn sql_weight(column: &str, now: &str) -> String {
    use std::fmt::Write as _;
    let mut sql = String::from("CASE");
    for (bound, weight) in AGE_WEIGHTS {
        let _ = write!(sql, " WHEN {now} - {column} < {bound} THEN {weight:?}");
    }
    let _ = write!(sql, " ELSE {OLDER_WEIGHT:?} END");
    sql
}

/// The exit status of a command stopped with Ctrl+C (128 + SIGINT).
pub const INTERRUPTED: i32 = 130;

/// A SQL condition that is true when the exit status in `column` counts as a
/// failure when judging how often a command fails: known, not zero, and not
/// [`INTERRUPTED`] — Ctrl+C is how a dev server or `tail -f` normally ends,
/// so it is not held against the command.
pub fn sql_failed(column: &str) -> String {
    format!("({column} IS NOT NULL AND {column} NOT IN (0, {INTERRUPTED}))")
}

/// One command's history, as recall's Smart rank weighs it.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct CommandStats {
    /// The sum of [`run_weight`] over its runs.
    pub frecency: f64,
    /// Runs whose exit status is known.
    pub outcomes: u32,
    /// Of those, the ones that failed (see [`sql_failed`]).
    pub failures: u32,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn weights_fall_with_age() {
        assert!((run_weight(0) - 16.0).abs() < f64::EPSILON);
        assert!((run_weight(HOUR_MS - 1) - 16.0).abs() < f64::EPSILON);
        assert!((run_weight(HOUR_MS) - 8.0).abs() < f64::EPSILON);
        assert!((run_weight(2 * DAY_MS) - 4.0).abs() < f64::EPSILON);
        assert!((run_weight(5 * DAY_MS) - 1.0).abs() < f64::EPSILON);
        assert!((run_weight(10 * DAY_MS) - 0.5).abs() < f64::EPSILON);
        assert!((run_weight(400 * DAY_MS) - 0.25).abs() < f64::EPSILON);
        // A clock that moved back is "just now", not a negative weight.
        assert!((run_weight(-5_000) - 16.0).abs() < f64::EPSILON);
    }

    #[test]
    fn the_sql_expression_agrees_with_run_weight() {
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        let sql = format!("SELECT {}", sql_weight("?2", "?1"));
        let now = 1_000 * DAY_MS;
        for age in [
            -1,
            0,
            HOUR_MS - 1,
            HOUR_MS,
            DAY_MS + 7,
            3 * DAY_MS,
            6 * DAY_MS,
            7 * DAY_MS,
            29 * DAY_MS,
            30 * DAY_MS,
            900 * DAY_MS,
        ] {
            let got: f64 = conn
                .query_row(&sql, rusqlite::params![now, now - age], |r| r.get(0))
                .unwrap();
            assert!(
                (got - run_weight(age)).abs() < f64::EPSILON,
                "age {age}: sql {got}, rust {}",
                run_weight(age)
            );
        }
    }

    #[test]
    fn an_interrupted_or_unknown_run_is_not_a_failure() {
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        let sql = format!("SELECT {}", sql_failed("?1"));
        let failed =
            |code: Option<i32>| -> bool { conn.query_row(&sql, [code], |r| r.get(0)).unwrap() };
        assert!(!failed(Some(0)));
        assert!(!failed(Some(INTERRUPTED)));
        assert!(!failed(None));
        assert!(failed(Some(1)));
        assert!(failed(Some(127)));
        assert!(failed(Some(-1)));
    }
}
