//! Frecency: how often *and* how recently a command was run, as one number.
//!
//! Every run of a command adds a weight that depends only on how long ago it
//! started, and a command's frecency is the sum over its runs. One run in the
//! last hour is worth sixteen runs from last week, so a habit you have kept up
//! today outweighs one you dropped, while a command you have run hundreds of
//! times keeps a lead over something you ran once.
//!
//! This is the single definition of frecency in Suvadu; the MCP
//! `suggest_next` tool sums [`run_weight`] over recent runs.

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
}
