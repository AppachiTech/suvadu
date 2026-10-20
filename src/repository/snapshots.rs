//! Git snapshots of agent sessions (`ai_snapshots`, schema v15).
//!
//! Rows arrive already filtered and redacted by
//! [`crate::ai_sessions::git_snapshot::GitSnapshot::from_status`]; this
//! module stores and reads them. A session has at most two: the `start`
//! one, kept from the first prompt Suvadu saw, and the `end` one, replaced
//! at every turn end. They are keyed by the session id commands and
//! transcripts use (`claude-<id>`, `codex-<id>`, `opencode-<id>`), with no
//! foreign key, because the first snapshot is taken before either exists;
//! `suv agent delete-session` deletes them with the session and `suv gc`
//! removes any left without one.

use rusqlite::{params, OptionalExtension};

use crate::ai_sessions::git_snapshot::{ChangedPath, GitSnapshot, SnapshotKind};
use crate::db::{DbError, DbResult};

use super::Repository;

/// How old a snapshot without a session must be before `suv gc` removes
/// it: a session's first snapshot is taken before its first command or
/// transcript import, so a younger one may belong to a session in progress.
pub const ORPHAN_SNAPSHOT_AGE_MS: i64 = 24 * 60 * 60 * 1000;

const SNAPSHOT_COLUMNS: &str =
    "session_id, kind, at, cwd, repo_root, head, branch, paths, path_count, omitted, capped";

/// The condition, over `ai_snapshots` aliased `g`, for a snapshot whose
/// session left no other record.
const ORPHANED: &str = "NOT EXISTS (SELECT 1 FROM ai_sessions a WHERE a.id = g.session_id)
     AND NOT EXISTS (SELECT 1 FROM ai_sources r WHERE r.session_id = g.session_id)
     AND NOT EXISTS (SELECT 1 FROM entries e WHERE e.session_id = g.session_id)";

fn snapshot_from_row(row: &rusqlite::Row) -> rusqlite::Result<GitSnapshot> {
    let kind: String = row.get(1)?;
    let paths: String = row.get(7)?;
    let paths: Vec<ChangedPath> = serde_json::from_str(&paths).map_err(|e| {
        rusqlite::Error::FromSqlConversionFailure(7, rusqlite::types::Type::Text, Box::new(e))
    })?;
    Ok(GitSnapshot {
        session_id: row.get(0)?,
        kind: SnapshotKind::parse(&kind).ok_or_else(|| {
            rusqlite::Error::FromSqlConversionFailure(
                1,
                rusqlite::types::Type::Text,
                format!("unknown snapshot kind {kind:?}").into(),
            )
        })?,
        at: row.get(2)?,
        cwd: row.get(3)?,
        repo_root: row.get(4)?,
        head: row.get(5)?,
        branch: row.get(6)?,
        paths,
        path_count: usize::try_from(row.get::<_, i64>(8)?).unwrap_or(0),
        omitted: usize::try_from(row.get::<_, i64>(9)?).unwrap_or(0),
        capped: row.get(10)?,
    })
}

impl Repository {
    /// Store `snapshot`. A `start` snapshot is kept only if the session has
    /// none yet; an `end` one replaces the previous one.
    pub fn save_git_snapshot(&self, snapshot: &GitSnapshot) -> DbResult<()> {
        let paths = serde_json::to_string(&snapshot.paths)
            .map_err(|e| DbError::Validation(format!("cannot encode snapshot paths: {e}")))?;
        let conflict = match snapshot.kind {
            SnapshotKind::Start => "DO NOTHING",
            SnapshotKind::End => {
                "DO UPDATE SET at = excluded.at, cwd = excluded.cwd,
                    repo_root = excluded.repo_root, head = excluded.head,
                    branch = excluded.branch, paths = excluded.paths,
                    path_count = excluded.path_count, omitted = excluded.omitted,
                    capped = excluded.capped"
            }
        };
        self.conn.execute(
            &format!(
                "INSERT INTO ai_snapshots ({SNAPSHOT_COLUMNS})
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)
                 ON CONFLICT(session_id, kind) {conflict}"
            ),
            params![
                snapshot.session_id,
                snapshot.kind.as_str(),
                snapshot.at,
                snapshot.cwd,
                snapshot.repo_root,
                snapshot.head,
                snapshot.branch,
                paths,
                i64::try_from(snapshot.path_count).unwrap_or(i64::MAX),
                i64::try_from(snapshot.omitted).unwrap_or(i64::MAX),
                snapshot.capped,
            ],
        )?;
        Ok(())
    }

    /// Whether the session already has a snapshot of `kind`.
    pub fn has_git_snapshot(&self, session_id: &str, kind: SnapshotKind) -> DbResult<bool> {
        Ok(self.conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM ai_snapshots WHERE session_id = ?1 AND kind = ?2)",
            params![session_id, kind.as_str()],
            |row| row.get(0),
        )?)
    }

    /// The session's start and end snapshots. A row that cannot be read
    /// counts as missing.
    pub fn git_snapshots(
        &self,
        session_id: &str,
    ) -> DbResult<(Option<GitSnapshot>, Option<GitSnapshot>)> {
        let mut stmt = self.conn.prepare_cached(&format!(
            "SELECT {SNAPSHOT_COLUMNS} FROM ai_snapshots WHERE session_id = ?1 AND kind = ?2"
        ))?;
        let mut read = |kind: SnapshotKind| -> DbResult<Option<GitSnapshot>> {
            Ok(stmt
                .query_row(params![session_id, kind.as_str()], snapshot_from_row)
                .optional()
                .or_else(|e| match e {
                    rusqlite::Error::FromSqlConversionFailure(..) => Ok(None),
                    other => Err(other),
                })?)
        };
        Ok((read(SnapshotKind::Start)?, read(SnapshotKind::End)?))
    }

    /// Sessions whose end snapshot was taken at or after `after`, newest
    /// first, at most `limit`.
    pub fn git_snapshot_sessions(&self, after: i64, limit: usize) -> DbResult<Vec<String>> {
        let mut stmt = self.conn.prepare_cached(
            "SELECT session_id FROM ai_snapshots WHERE kind = 'end' AND at >= ?1
             ORDER BY at DESC, session_id LIMIT ?2",
        )?;
        let rows = stmt.query_map(
            params![after, i64::try_from(limit).unwrap_or(i64::MAX)],
            |row| row.get(0),
        )?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    /// Snapshots older than `older_than` (epoch ms) whose session left no
    /// other record.
    pub fn count_orphaned_git_snapshots(&self, older_than: i64) -> DbResult<i64> {
        Ok(self.conn.query_row(
            &format!("SELECT COUNT(*) FROM ai_snapshots g WHERE g.at < ?1 AND {ORPHANED}"),
            [older_than],
            |row| row.get(0),
        )?)
    }

    /// Delete what [`Self::count_orphaned_git_snapshots`] counts. Returns
    /// how many went.
    pub fn delete_orphaned_git_snapshots(&self, older_than: i64) -> DbResult<usize> {
        Ok(self.conn.execute(
            &format!(
                "DELETE FROM ai_snapshots WHERE rowid IN
                 (SELECT g.rowid FROM ai_snapshots g WHERE g.at < ?1 AND {ORPHANED})"
            ),
            [older_than],
        )?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::{Entry, Session};
    use crate::test_utils::test_repo;

    fn shot(session: &str, kind: SnapshotKind, at: i64, head: &str) -> GitSnapshot {
        GitSnapshot {
            session_id: session.into(),
            kind,
            at,
            cwd: "/repo/src".into(),
            repo_root: "/repo".into(),
            head: Some(head.into()),
            branch: Some("main".into()),
            paths: vec![ChangedPath {
                status: "R.".into(),
                path: "new.rs".into(),
                from: Some("old.rs".into()),
            }],
            path_count: 3,
            omitted: 1,
            capped: true,
        }
    }

    #[test]
    fn the_first_start_and_the_latest_end_are_kept() {
        let (_dir, repo) = test_repo();
        assert!(!repo
            .has_git_snapshot("claude-s", SnapshotKind::Start)
            .unwrap());
        repo.save_git_snapshot(&shot("claude-s", SnapshotKind::Start, 1, "aaaa"))
            .unwrap();
        repo.save_git_snapshot(&shot("claude-s", SnapshotKind::Start, 2, "bbbb"))
            .unwrap();
        repo.save_git_snapshot(&shot("claude-s", SnapshotKind::End, 3, "cccc"))
            .unwrap();
        repo.save_git_snapshot(&shot("claude-s", SnapshotKind::End, 4, "dddd"))
            .unwrap();
        assert!(repo
            .has_git_snapshot("claude-s", SnapshotKind::Start)
            .unwrap());
        let (start, end) = repo.git_snapshots("claude-s").unwrap();
        assert_eq!(
            start,
            Some(shot("claude-s", SnapshotKind::Start, 1, "aaaa"))
        );
        assert_eq!(end, Some(shot("claude-s", SnapshotKind::End, 4, "dddd")));
        assert_eq!(repo.git_snapshots("claude-other").unwrap(), (None, None));
    }

    #[test]
    fn sessions_are_listed_by_their_latest_end_snapshot() {
        let (_dir, repo) = test_repo();
        repo.save_git_snapshot(&shot("a", SnapshotKind::End, 100, "aaaa"))
            .unwrap();
        repo.save_git_snapshot(&shot("b", SnapshotKind::End, 300, "aaaa"))
            .unwrap();
        repo.save_git_snapshot(&shot("c", SnapshotKind::Start, 400, "aaaa"))
            .unwrap();
        repo.save_git_snapshot(&shot("d", SnapshotKind::End, 50, "aaaa"))
            .unwrap();
        assert_eq!(repo.git_snapshot_sessions(100, 10).unwrap(), ["b", "a"]);
        assert_eq!(repo.git_snapshot_sessions(0, 1).unwrap(), ["b"]);
    }

    #[test]
    fn an_unreadable_row_reads_as_missing() {
        let (_dir, repo) = test_repo();
        repo.save_git_snapshot(&shot("s", SnapshotKind::End, 1, "aaaa"))
            .unwrap();
        repo.conn
            .execute("UPDATE ai_snapshots SET paths = 'not json'", [])
            .unwrap();
        assert_eq!(repo.git_snapshots("s").unwrap(), (None, None));
    }

    #[test]
    fn deleting_an_agent_session_deletes_its_snapshots() {
        let (_dir, repo) = test_repo();
        repo.save_git_snapshot(&shot("claude-s", SnapshotKind::Start, 1, "aaaa"))
            .unwrap();
        repo.save_git_snapshot(&shot("claude-s", SnapshotKind::End, 2, "bbbb"))
            .unwrap();
        repo.save_git_snapshot(&shot("claude-keep", SnapshotKind::End, 2, "bbbb"))
            .unwrap();
        // A session known only by its snapshots can still be deleted.
        let preview = repo.ai_session_deletion_preview("claude-s").unwrap();
        assert_eq!(preview.snapshots, 2);
        repo.delete_ai_session("claude-s").unwrap();
        assert_eq!(repo.git_snapshots("claude-s").unwrap(), (None, None));
        assert!(repo.git_snapshots("claude-keep").unwrap().1.is_some());
        assert!(repo.delete_ai_session("claude-s").is_err());
    }

    #[test]
    fn gc_removes_only_old_snapshots_without_a_session() {
        let (_dir, repo) = test_repo();
        repo.insert_session(&Session {
            id: "claude-live".into(),
            hostname: "h".into(),
            created_at: 1,
            tag_id: None,
        })
        .unwrap();
        repo.insert_entry(&Entry::new(
            "claude-live".into(),
            "ls".into(),
            "/repo".into(),
            Some(0),
            1,
            2,
        ))
        .unwrap();
        repo.save_git_snapshot(&shot("claude-live", SnapshotKind::End, 10, "aaaa"))
            .unwrap();
        repo.save_git_snapshot(&shot("claude-gone", SnapshotKind::Start, 10, "aaaa"))
            .unwrap();
        repo.save_git_snapshot(&shot("claude-gone", SnapshotKind::End, 20, "aaaa"))
            .unwrap();
        repo.save_git_snapshot(&shot("claude-young", SnapshotKind::Start, 900, "aaaa"))
            .unwrap();
        assert_eq!(repo.count_orphaned_git_snapshots(500).unwrap(), 2);
        assert_eq!(repo.delete_orphaned_git_snapshots(500).unwrap(), 2);
        assert!(repo.git_snapshots("claude-live").unwrap().1.is_some());
        assert!(repo.git_snapshots("claude-young").unwrap().0.is_some());
        assert_eq!(repo.git_snapshots("claude-gone").unwrap(), (None, None));
    }
}
