//! The history recall's Smart rank weighs a command by: how often and how
//! recently it ran, and how often it failed.
//!
//! Every query here binds user text as a parameter; the only SQL built at
//! run time is from constants (the frecency table and the executor types).

use std::collections::HashMap;

use crate::db::DbResult;
use crate::frecency::{self, CommandStats};

use super::Repository;

/// `alias.executor_type` is not an agent, bot, CI or script executor — the
/// rows recall shows while agent commands are hidden.
fn interactive_clause(alias: &str) -> String {
    let list = crate::models::ExecutorKind::NON_INTERACTIVE_DB_VALUES
        .iter()
        .map(|v| format!("'{v}'"))
        .collect::<Vec<_>>()
        .join(", ");
    format!("({alias}.executor_type IS NULL OR {alias}.executor_type NOT IN ({list}))")
}

impl Repository {
    /// The query behind [`Self::command_stats`]: `?1` is now, `?2` the
    /// commands as a JSON array.
    pub(super) fn command_stats_sql(include_agents: bool) -> String {
        let agents = if include_agents {
            String::new()
        } else {
            format!(" AND {}", interactive_clause("e"))
        };
        format!(
            "SELECT e.command, SUM({weight}), COUNT(e.exit_code), \
                    SUM(CASE WHEN {failed} THEN 1 ELSE 0 END) \
             FROM entries e \
             WHERE e.command IN (SELECT value FROM json_each(?2)){agents} \
             GROUP BY e.command",
            weight = frecency::sql_weight("e.started_at", "?1"),
            failed = frecency::sql_failed("e.exit_code"),
        )
    }

    /// Frecency and outcome counts for each of `commands` that has run,
    /// over every recorded run of exactly that text, as of `now`. Agent,
    /// bot, CI and script runs count only when `include_agents` is set, so
    /// hidden commands never move what is shown.
    ///
    /// One indexed lookup per command (`idx_entries_command_started`).
    pub fn command_stats(
        &self,
        commands: &[&str],
        now: i64,
        include_agents: bool,
    ) -> DbResult<HashMap<String, CommandStats>> {
        if commands.is_empty() {
            return Ok(HashMap::new());
        }
        let sql = Self::command_stats_sql(include_agents);
        let list = serde_json::to_string(commands).map_err(|e| {
            crate::db::DbError::Validation(format!("failed to encode commands: {e}"))
        })?;
        let mut stmt = self.conn.prepare_cached(&sql)?;
        let rows = stmt.query_map(rusqlite::params![now, list], |row| {
            Ok((
                row.get::<_, String>(0)?,
                CommandStats {
                    frecency: row.get(1)?,
                    outcomes: row.get(2)?,
                    failures: row.get(3)?,
                },
            ))
        })?;
        rows.collect::<Result<_, _>>().map_err(Into::into)
    }
}
