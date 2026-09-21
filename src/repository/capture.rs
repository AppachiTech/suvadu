//! Telling *stored* shell history apart from history a live hook was
//! observed capturing.
//!
//! `suv status` and `suv doctor` may only claim capture when a row could
//! have been written by the recording hook. Imported rows are ordinary,
//! searchable history — and an import can legitimately carry a timestamp of
//! "now" — so counting recent rows says nothing at all about whether the
//! hook is installed and running.

use crate::db::DbResult;
use rusqlite::params;

use super::Repository;

/// Hostname the JSONL importer stamps on a session it had to invent because
/// the export came from another machine. Declared here as well as at the
/// import site so the diagnostics query and the importer cannot drift.
pub const PLACEHOLDER_IMPORT_HOSTNAME: &str = "imported";

/// Context key the JSONL importer stamps on every row it writes, recording
/// when *this* database received it.
///
/// It is deliberately not `import_source`/`imported_at`: those keys are kept
/// as the export wrote them so a restored row still says where the command
/// was originally recorded, and overwriting them would lose that. This key
/// is the one addition, and it replaces itself if an export already carried
/// one — it describes *this* database's copy, not the original.
pub const RESTORED_AT_KEY: &str = "restored_at";

/// SQL that is true for a row an importer wrote.
///
/// Four independent signals, because no single one covers every importer or
/// every row already sitting in an older database:
///
/// * `context.import_source` — written by the Bash, zsh and Atuin importers.
///   The zsh importer only started writing it alongside this check, so rows
///   it wrote before that are caught by the session-id rule below.
/// * `context.imported_at` — the same provenance, recorded independently, so
///   a row that somehow lost one key is still recognised by the other.
/// * the importer's own session id — `import-bash-…`, `import-zsh-…` and
///   `atuin-…` are namespaces no live hook can produce (a hook session id is
///   a UUID, and `suv add` rejects anything that is not a valid session id).
/// * the JSONL importer's placeholder session hostname, which is how an
///   export from another machine lands here. That importer keeps the
///   exported `context` rather than replacing it, so the row's own
///   provenance — where it was *originally* recorded — survives; the one
///   key it adds is `restored_at`, below.
/// * `context.restored_at`, which that importer stamps on every row it
///   writes. The hostname rule alone only catches a session the importer
///   created, so a restore into an existing session used to look live.
///
/// Every comparison is NULL-safe: a signal that is unknown must read as
/// "not imported", never as NULL, or the negation would silently drop rows.
pub const IMPORTED_ROW_SQL: &str = "(\
     json_extract(e.context, '$.import_source') IS NOT NULL \
     OR json_extract(e.context, '$.imported_at') IS NOT NULL \
     OR e.session_id LIKE 'import-bash-%' \
     OR e.session_id LIKE 'import-zsh-%' \
     OR e.session_id LIKE 'atuin-%' \
     OR json_extract(e.context, '$.restored_at') IS NOT NULL \
     OR IFNULL(s.hostname, '') = 'imported')";

/// The newest stored shell record, for display.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CaptureRecord {
    pub command: String,
    pub started_at: i64,
    pub exit_code: Option<i32>,
    /// `true` when this row came from an import rather than a live hook.
    pub imported: bool,
}

/// What the stored rows can and cannot support, counted separately by
/// provenance and by shell session.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CaptureRecordStats {
    /// Non-agent rows that no importer wrote — i.e. rows only the live hook
    /// could have produced.
    pub live_records: i64,
    /// `started_at` of the newest such row.
    pub newest_live_started_at: Option<i64>,
    /// Live rows recorded in the shell session being diagnosed.
    pub session_records: i64,
    pub newest_session_started_at: Option<i64>,
    /// Non-agent rows an importer wrote. Searchable history, never evidence.
    pub imported_records: i64,
    /// Newest non-agent row of either provenance.
    pub newest_record: Option<CaptureRecord>,
}

impl Repository {
    /// Count shell (non-agent) rows by provenance, and find the newest one.
    ///
    /// `session_id` is the shell session being diagnosed (`SUVADU_SESSION_ID`);
    /// `None` means the caller was not run from a hooked shell, in which case
    /// no row can be attributed to it.
    pub fn capture_record_stats(&self, session_id: Option<&str>) -> DbResult<CaptureRecordStats> {
        let automated = crate::models::ExecutorKind::NON_INTERACTIVE_DB_VALUES
            .iter()
            .map(|value| format!("'{value}'"))
            .collect::<Vec<_>>()
            .join(", ");
        // Hardcoded constants only — no user input is interpolated anywhere
        // in this module; the session id is bound as a parameter.
        let shell_rows = format!(
            "FROM entries e JOIN sessions s ON e.session_id = s.id \
             WHERE (e.executor_type IS NULL OR e.executor_type NOT IN ({automated}))"
        );

        let (live_records, newest_live_started_at) = self.conn.query_row(
            &format!("SELECT COUNT(*), MAX(e.started_at) {shell_rows} AND NOT {IMPORTED_ROW_SQL}"),
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;

        let imported_records = self.conn.query_row(
            &format!("SELECT COUNT(*) {shell_rows} AND {IMPORTED_ROW_SQL}"),
            [],
            |row| row.get(0),
        )?;

        let (session_records, newest_session_started_at) = match session_id {
            Some(session) => self.conn.query_row(
                &format!(
                    "SELECT COUNT(*), MAX(e.started_at) {shell_rows} \
                     AND NOT {IMPORTED_ROW_SQL} AND e.session_id = ?1"
                ),
                params![session],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )?,
            None => (0, None),
        };

        let newest_record = self
            .conn
            .query_row(
                &format!(
                    "SELECT e.command, e.started_at, e.exit_code, {IMPORTED_ROW_SQL} \
                     {shell_rows} ORDER BY e.started_at DESC LIMIT 1"
                ),
                [],
                |row| {
                    Ok(CaptureRecord {
                        command: row.get(0)?,
                        started_at: row.get(1)?,
                        exit_code: row.get(2)?,
                        imported: row.get(3)?,
                    })
                },
            )
            .ok();

        Ok(CaptureRecordStats {
            live_records,
            newest_live_started_at,
            session_records,
            newest_session_started_at,
            imported_records,
            newest_record,
        })
    }
}

#[cfg(test)]
mod tests {
    use crate::models::Entry;
    use crate::repository::Repository;

    fn live(repo: &Repository, session: &str, command: &str, at: i64) {
        repo.insert_session_if_missing(session, "laptop", at)
            .unwrap();
        let mut entry = Entry::new(
            session.to_string(),
            command.to_string(),
            "/work".to_string(),
            Some(0),
            at,
            at,
        );
        entry.executor_type = Some("human".to_string());
        repo.insert_entry(&entry).unwrap();
    }

    #[test]
    fn a_hook_written_row_is_live_and_attributed_to_its_session() {
        let (_dir, repo) = crate::test_utils::test_repo();
        live(&repo, "shell-a", "echo hi", 1_000);

        let stats = repo.capture_record_stats(Some("shell-a")).unwrap();
        assert_eq!(stats.live_records, 1);
        assert_eq!(stats.imported_records, 0);
        assert_eq!(stats.session_records, 1);
        assert_eq!(stats.newest_session_started_at, Some(1_000));

        let elsewhere = repo.capture_record_stats(Some("shell-b")).unwrap();
        assert_eq!(elsewhere.live_records, 1, "the row is still live");
        assert_eq!(
            elsewhere.session_records, 0,
            "but it belongs to another shell session"
        );

        let no_session = repo.capture_record_stats(None).unwrap();
        assert_eq!(no_session.session_records, 0);
    }

    /// Every way an imported row can reach the database must be recognised,
    /// including rows an older suvadu wrote without provenance context.
    #[test]
    fn every_import_shape_is_recognised_as_stored_history() {
        for (case, session, hostname, context) in [
            (
                "bash importer",
                "import-bash-1",
                "laptop",
                Some(r#"{"import_source":"bash-history","imported_at":"5"}"#),
            ),
            ("zsh importer, older suvadu", "import-zsh-1", "laptop", None),
            (
                "zsh importer",
                "import-zsh-2",
                "laptop",
                Some(r#"{"import_source":"zsh-history","imported_at":"5"}"#),
            ),
            (
                "atuin importer",
                "atuin-0193c0ffee",
                "laptop",
                Some(r#"{"import_source":"atuin-db","imported_at":"5"}"#),
            ),
            ("jsonl from another machine", "remote-1", "imported", None),
        ] {
            let (_dir, repo) = crate::test_utils::test_repo();
            repo.insert_session_if_missing(session, hostname, 1)
                .unwrap();
            repo.raw_execute_for_test(&format!(
                "INSERT INTO entries (session_id, command, cwd, exit_code, started_at, ended_at, duration_ms, context)
                 VALUES ('{session}', 'echo imported', '', NULL, 1000, 1000, 0, {})",
                context.map_or_else(|| "NULL".to_string(), |c| format!("'{c}'"))
            ))
            .unwrap();

            let stats = repo.capture_record_stats(Some(session)).unwrap();
            assert_eq!(stats.imported_records, 1, "{case}: not seen as imported");
            assert_eq!(stats.live_records, 0, "{case}: counted as live capture");
            assert_eq!(
                stats.session_records, 0,
                "{case}: an imported row must never be attributed to a live session"
            );
            assert_eq!(stats.newest_live_started_at, None, "{case}");
            assert!(
                stats.newest_record.is_some_and(|r| r.imported),
                "{case}: the newest row must be labelled imported"
            );
        }
    }

    #[test]
    fn agent_rows_are_not_shell_capture_evidence() {
        let (_dir, repo) = crate::test_utils::test_repo();
        repo.insert_session_if_missing("agent-1", "laptop", 1)
            .unwrap();
        let mut entry = Entry::new(
            "agent-1".to_string(),
            "ls".to_string(),
            "/work".to_string(),
            Some(0),
            1_000,
            1_000,
        );
        entry.executor_type = Some("agent".to_string());
        repo.insert_entry(&entry).unwrap();

        let stats = repo.capture_record_stats(Some("agent-1")).unwrap();
        assert_eq!(stats.live_records, 0);
        assert_eq!(stats.imported_records, 0);
        assert_eq!(stats.newest_record, None);
    }
}
