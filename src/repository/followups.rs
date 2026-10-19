//! The two reads that pairing a failure with what fixed it needs (see the
//! binary's `fixes` module): the recent failures of one command, and what
//! ran after a command in its own session. Both are index seeks; nothing
//! here scans a table.

use crate::db::DbResult;

use super::{FilterBuilder, Repository};

/// One recorded run, with what fix pairing reads of it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Run {
    pub id: i64,
    pub session_id: String,
    pub command: String,
    pub cwd: String,
    pub exit_code: Option<i32>,
    pub started_at: i64,
    pub ended_at: i64,
}

const RUN_COLUMNS: &str =
    "e.id, e.session_id, e.command, e.cwd, e.exit_code, e.started_at, e.ended_at";

fn run_from_row(row: &rusqlite::Row) -> rusqlite::Result<Run> {
    Ok(Run {
        id: row.get(0)?,
        session_id: row.get(1)?,
        command: row.get(2)?,
        cwd: row.get(3)?,
        exit_code: row.get(4)?,
        started_at: row.get(5)?,
        ended_at: row.get(6)?,
    })
}

impl Repository {
    /// The newest `sample` runs of exactly `command` that failed — a known,
    /// non-zero exit other than 130 (Ctrl+C) — newest first, leaving out
    /// runs in `exclude_dirs` (as `mcp.exclude_dirs` matches them).
    ///
    /// Reads `idx_entries_command_started`, which holds the exit code, so
    /// only the failures cost a lookup in the table.
    pub fn failed_runs_of(
        &self,
        command: &str,
        sample: usize,
        exclude_dirs: &[String],
    ) -> DbResult<Vec<Run>> {
        let mut filter = FilterBuilder::new().with_excluded_dirs(exclude_dirs);
        let sql = format!(
            "SELECT {RUN_COLUMNS} FROM entries e{} AND e.command = ? AND {} \
             ORDER BY e.started_at DESC, e.id DESC LIMIT ?",
            filter.build_where(),
            crate::frecency::sql_failed("e.exit_code"),
        );
        filter.push_param(Box::new(command.to_string()));
        filter.push_param(Box::new(i64::try_from(sample).unwrap_or(i64::MAX)));
        let mut stmt = self.conn.prepare_cached(&sql)?;
        let rows = stmt.query_map(
            rusqlite::params_from_iter(filter.params_refs()),
            run_from_row,
        )?;
        rows.collect::<Result<_, _>>().map_err(Into::into)
    }

    /// Up to `limit` runs recorded in `session_id` after the run `after_id`,
    /// in the order they were recorded.
    ///
    /// Commands in one session are recorded one after another, so what came
    /// next is the next ids in the session: a seek on
    /// `idx_entries_session_id`, whose entries are in id order within a
    /// session, rather than a sort of the whole session.
    pub fn runs_after(&self, session_id: &str, after_id: i64, limit: usize) -> DbResult<Vec<Run>> {
        let mut stmt = self.conn.prepare_cached(&format!(
            "SELECT {RUN_COLUMNS} FROM entries e INDEXED BY idx_entries_session_id \
             WHERE e.session_id = ?1 AND e.id > ?2 ORDER BY e.id LIMIT ?3"
        ))?;
        let rows = stmt.query_map(
            rusqlite::params![
                session_id,
                after_id,
                i64::try_from(limit).unwrap_or(i64::MAX)
            ],
            run_from_row,
        )?;
        rows.collect::<Result<_, _>>().map_err(Into::into)
    }
}

#[cfg(test)]
mod tests {
    use crate::models::{Entry, Session};
    use crate::test_utils::test_repo;

    fn record(
        repo: &crate::repository::Repository,
        session: &str,
        cwd: &str,
        command: &str,
        exit: Option<i32>,
        at: i64,
    ) -> i64 {
        if repo.get_session(session).unwrap().is_none() {
            repo.insert_session(&Session {
                id: session.to_string(),
                hostname: "host".to_string(),
                created_at: 0,
                tag_id: None,
            })
            .unwrap();
        }
        repo.insert_entry(&Entry::new(
            session.to_string(),
            command.to_string(),
            cwd.to_string(),
            exit,
            at,
            at + 100,
        ))
        .unwrap()
    }

    #[test]
    fn failed_runs_are_the_newest_failures_of_exactly_that_command() {
        let (_dir, repo) = test_repo();
        record(&repo, "s1", "/p", "make", Some(2), 1_000);
        record(&repo, "s1", "/p", "make", Some(0), 2_000);
        record(&repo, "s1", "/p", "make", Some(130), 3_000);
        record(&repo, "s1", "/p", "make", None, 4_000);
        record(&repo, "s1", "/p", "make test", Some(2), 5_000);
        let newest = record(&repo, "s2", "/p", "make", Some(1), 6_000);
        let runs = repo.failed_runs_of("make", 10, &[]).unwrap();
        let starts: Vec<i64> = runs.iter().map(|r| r.started_at).collect();
        assert_eq!(starts, [6_000, 1_000]);
        assert_eq!(runs[0].id, newest);
        assert_eq!(runs[0].session_id, "s2");
        assert_eq!(repo.failed_runs_of("make", 1, &[]).unwrap().len(), 1);
    }

    #[test]
    fn failed_runs_leave_out_excluded_directories() {
        let (_dir, repo) = test_repo();
        record(&repo, "s1", "/secret/app", "make", Some(2), 1_000);
        record(&repo, "s1", "/p", "make", Some(2), 2_000);
        let runs = repo
            .failed_runs_of("make", 10, &["/secret".to_string()])
            .unwrap();
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].cwd, "/p");
    }

    #[test]
    fn runs_after_are_the_sessions_next_commands_in_order() {
        let (_dir, repo) = test_repo();
        let first = record(&repo, "s1", "/p", "npm test", Some(1), 1_000);
        record(&repo, "s2", "/p", "elsewhere", Some(0), 1_500);
        record(&repo, "s1", "/p", "npm install", Some(0), 2_000);
        record(&repo, "s1", "/p", "npm test", Some(0), 3_000);
        let after = repo.runs_after("s1", first, 10).unwrap();
        let commands: Vec<&str> = after.iter().map(|r| r.command.as_str()).collect();
        assert_eq!(commands, ["npm install", "npm test"]);
        assert_eq!(repo.runs_after("s1", first, 1).unwrap().len(), 1);
    }

    #[test]
    fn both_reads_seek_an_index() {
        let (_dir, repo) = test_repo();
        let plan = |sql: &str| -> String {
            let mut stmt = repo
                .conn
                .prepare(&format!("EXPLAIN QUERY PLAN {sql}"))
                .unwrap();
            let rows: Vec<String> = stmt
                .query_map([], |row| row.get::<_, String>(3))
                .unwrap()
                .collect::<Result<_, _>>()
                .unwrap();
            rows.join("\n")
        };
        let failed = plan(
            "SELECT e.id FROM entries e WHERE 1=1 AND e.command = 'make' \
             AND (e.exit_code IS NOT NULL AND e.exit_code NOT IN (0, 130)) \
             ORDER BY e.started_at DESC, e.id DESC LIMIT 50",
        );
        assert!(failed.contains("idx_entries_command_started"), "{failed}");
        let after = plan(
            "SELECT e.id FROM entries e INDEXED BY idx_entries_session_id \
             WHERE e.session_id = 's1' AND e.id > 5 ORDER BY e.id LIMIT 20",
        );
        assert!(after.contains("idx_entries_session_id"), "{after}");
        assert!(!after.contains("TEMP B-TREE"), "no sort: {after}");
    }
}
