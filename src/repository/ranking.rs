//! The history recall's Smart rank weighs a command by: how often and how
//! recently it ran, how often it failed, and what tends to run after the
//! command before it.
//!
//! Every query here binds user text as a parameter; the only SQL built at
//! run time is from constants (the frecency table and the executor types).

use std::collections::HashMap;

use rusqlite::OptionalExtension;

use crate::db::DbResult;
use crate::frecency::{self, CommandStats};

use super::Repository;

/// What ran next, in the same shell session, after recent runs of one
/// command. See [`Repository::followers_of`].
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Followers {
    /// Runs of the command that something followed within the window.
    pub occurrences: u32,
    /// How many of those each command followed.
    pub next: HashMap<String, u32>,
}

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

    /// The query behind [`Self::followers_of`]: `?1` is the command, `?2`
    /// the sample size, `?3` the window.
    pub(super) fn followers_sql(include_agents: bool) -> String {
        let (a_agents, b_agents) = if include_agents {
            (String::new(), String::new())
        } else {
            (
                format!(" AND {}", interactive_clause("e")),
                format!(" AND {}", interactive_clause("b")),
            )
        };
        format!(
            "SELECT b.command FROM ( \
                SELECT e.id, e.session_id, e.ended_at FROM entries e \
                WHERE e.command = ?1{a_agents} \
                ORDER BY e.started_at DESC, e.id DESC LIMIT ?2 \
             ) a \
             JOIN entries b ON b.id = ( \
                SELECT MIN(n.id) FROM entries n \
                WHERE n.session_id = a.session_id AND n.id > a.id \
             ) \
             WHERE b.started_at - a.ended_at <= ?3{b_agents}"
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

    /// The command run most recently in `session_id`, if any.
    pub fn last_command_in_session(&self, session_id: &str) -> DbResult<Option<String>> {
        Ok(self
            .conn
            .query_row(
                "SELECT command FROM entries WHERE session_id = ?1 \
                 ORDER BY started_at DESC, id DESC LIMIT 1",
                [session_id],
                |row| row.get(0),
            )
            .optional()?)
    }

    /// What ran next after the newest `sample` runs of `command`: for each
    /// one, the next command recorded in the same shell session, provided it
    /// started within `within_ms` of `command` finishing. Agent, bot, CI and
    /// script runs take part only when `include_agents` is set.
    ///
    /// Commands in one session are recorded one after another, so "next" is
    /// the next id in the session — an indexed seek per run, rather than a
    /// sort of the whole session.
    pub fn followers_of(
        &self,
        command: &str,
        within_ms: i64,
        sample: usize,
        include_agents: bool,
    ) -> DbResult<Followers> {
        let sql = Self::followers_sql(include_agents);
        let sample = i64::try_from(sample).unwrap_or(i64::MAX);
        let mut stmt = self.conn.prepare_cached(&sql)?;
        let mut rows = stmt.query(rusqlite::params![command, sample, within_ms])?;
        let mut followers = Followers::default();
        while let Some(row) = rows.next()? {
            followers.occurrences += 1;
            *followers.next.entry(row.get(0)?).or_default() += 1;
        }
        Ok(followers)
    }
}
