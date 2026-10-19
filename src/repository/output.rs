//! The end of what a failed command printed (`entry_output`, schema v14).
//!
//! Rows arrive already cleaned, redacted and cut by the capture path; this
//! module only stores and reads them. A row goes with its command through
//! the foreign key, and `suv gc` removes any a connection without foreign
//! keys left behind.

use rusqlite::{params, OptionalExtension};

use crate::db::DbResult;
use crate::models::FailureOutput;

use super::Repository;

impl Repository {
    /// Store `output` for the command `entry_id`, replacing any already
    /// stored for it.
    pub fn insert_failure_output(&self, entry_id: i64, output: &FailureOutput) -> DbResult<()> {
        self.conn.execute(
            "INSERT INTO entry_output (entry_id, stream, tail, truncated_bytes)
             VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(entry_id) DO UPDATE SET stream = excluded.stream,
                tail = excluded.tail, truncated_bytes = excluded.truncated_bytes",
            params![entry_id, output.stream, output.tail, output.truncated_bytes],
        )?;
        Ok(())
    }

    /// The output stored for the command `entry_id`, if any.
    pub fn failure_output(&self, entry_id: i64) -> DbResult<Option<FailureOutput>> {
        let mut stmt = self.conn.prepare_cached(
            "SELECT stream, tail, truncated_bytes FROM entry_output WHERE entry_id = ?1",
        )?;
        Ok(stmt
            .query_row([entry_id], |row| {
                Ok(FailureOutput {
                    stream: row.get(0)?,
                    tail: row.get(1)?,
                    truncated_bytes: row.get(2)?,
                })
            })
            .optional()?)
    }

    /// Stored outputs whose command no longer exists.
    pub fn count_orphaned_outputs(&self) -> DbResult<i64> {
        Ok(self.conn.query_row(
            "SELECT COUNT(*) FROM entry_output o
             WHERE NOT EXISTS (SELECT 1 FROM entries e WHERE e.id = o.entry_id)",
            [],
            |row| row.get(0),
        )?)
    }

    /// Delete stored outputs whose command no longer exists. Returns how
    /// many went.
    pub fn delete_orphaned_outputs(&self) -> DbResult<usize> {
        Ok(self.conn.execute(
            "DELETE FROM entry_output WHERE entry_id NOT IN (SELECT id FROM entries)",
            [],
        )?)
    }

    /// How many commands have a stored output.
    pub fn count_failure_outputs(&self) -> DbResult<i64> {
        Ok(self
            .conn
            .query_row("SELECT COUNT(*) FROM entry_output", [], |row| row.get(0))?)
    }

    /// Delete every stored output, keeping the commands. Returns how many
    /// went.
    pub fn delete_all_failure_outputs(&self) -> DbResult<usize> {
        Ok(self.conn.execute("DELETE FROM entry_output", [])?)
    }
}

#[cfg(test)]
mod tests {
    use crate::models::{Entry, FailureOutput, Session};
    use crate::test_utils::test_repo;

    fn output(tail: &str) -> FailureOutput {
        FailureOutput {
            stream: "stderr".to_string(),
            tail: tail.to_string(),
            truncated_bytes: 12,
        }
    }

    fn seed(repo: &crate::repository::Repository, commands: &[&str]) -> Vec<i64> {
        let session = Session::new("host".to_string(), 1000);
        repo.insert_session(&session).unwrap();
        commands
            .iter()
            .enumerate()
            .map(|(i, command)| {
                let i = i64::try_from(i).unwrap();
                repo.insert_entry(&Entry::new(
                    session.id.clone(),
                    (*command).to_string(),
                    "/work".to_string(),
                    Some(1),
                    1000 + i,
                    1100 + i,
                ))
                .unwrap()
            })
            .collect()
    }

    #[test]
    fn an_output_is_stored_read_back_and_replaced() {
        let (_dir, repo) = test_repo();
        let ids = seed(&repo, &["make", "make test"]);
        assert_eq!(repo.failure_output(ids[0]).unwrap(), None);
        repo.insert_failure_output(ids[0], &output("error: no rule"))
            .unwrap();
        assert_eq!(
            repo.failure_output(ids[0]).unwrap(),
            Some(output("error: no rule"))
        );
        repo.insert_failure_output(ids[0], &output("error: again"))
            .unwrap();
        assert_eq!(
            repo.failure_output(ids[0]).unwrap().unwrap().tail,
            "error: again"
        );
        assert_eq!(repo.count_failure_outputs().unwrap(), 1);
    }

    #[test]
    fn deleting_a_command_deletes_its_output() {
        let (_dir, repo) = test_repo();
        let ids = seed(&repo, &["make", "make test"]);
        for id in &ids {
            repo.insert_failure_output(*id, &output("boom")).unwrap();
        }
        repo.delete_entry(ids[0]).unwrap();
        assert_eq!(repo.failure_output(ids[0]).unwrap(), None);
        assert!(repo.failure_output(ids[1]).unwrap().is_some());
        // A pattern delete goes the same way.
        repo.delete_entries("make test", false, None).unwrap();
        assert_eq!(repo.count_failure_outputs().unwrap(), 0);
    }

    #[test]
    fn gc_finds_and_removes_outputs_left_without_their_command() {
        let (_dir, repo) = test_repo();
        let ids = seed(&repo, &["make"]);
        repo.insert_failure_output(ids[0], &output("boom")).unwrap();
        // A connection without foreign keys (the sqlite3 shell, say).
        repo.raw_execute_for_test("PRAGMA foreign_keys = OFF")
            .unwrap();
        repo.raw_execute_for_test("DELETE FROM entries").unwrap();
        repo.raw_execute_for_test("PRAGMA foreign_keys = ON")
            .unwrap();
        assert_eq!(repo.count_orphaned_outputs().unwrap(), 1);
        assert_eq!(repo.delete_orphaned_outputs().unwrap(), 1);
        assert_eq!(repo.count_orphaned_outputs().unwrap(), 0);
    }

    #[test]
    fn every_output_can_be_dropped_while_the_commands_stay() {
        let (_dir, repo) = test_repo();
        let ids = seed(&repo, &["make", "make test"]);
        for id in &ids {
            repo.insert_failure_output(*id, &output("boom")).unwrap();
        }
        assert_eq!(repo.delete_all_failure_outputs().unwrap(), 2);
        assert_eq!(repo.count_failure_outputs().unwrap(), 0);
        assert_eq!(repo.count_entries().unwrap(), 2);
    }
}
