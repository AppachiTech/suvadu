// SQL safety: all queries in this module use `FilterBuilder` to compose WHERE
// clauses. `FilterBuilder::build_where()` returns a string containing only
// hardcoded column names and `?` placeholders — never interpolated user values.
// Actual user input is stored in `FilterBuilder::params` and bound via rusqlite's
// parameterized query API (`params_refs()`). This is safe against SQL injection.

use crate::db::DbResult;
use crate::models::{Entry, SessionSummary};
use rusqlite::params;

use super::{entry_from_row, FilterBuilder, Repository, ENTRY_COLUMNS, ENTRY_JOINS};

impl Repository {
    /// Insert a new entry
    pub fn insert_entry(&self, entry: &Entry) -> DbResult<i64> {
        let context_json = entry
            .context
            .as_ref()
            .map(serde_json::to_string)
            .transpose()
            .map_err(|e| {
                crate::db::DbError::Validation(format!("failed to serialize entry context: {e}"))
            })?;

        self.conn.execute(
            "INSERT INTO entries (session_id, command, cwd, exit_code, started_at, ended_at, duration_ms, context, tag_id, executor_type, executor)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
            params![
                entry.session_id,
                entry.command,
                entry.cwd,
                entry.exit_code,
                entry.started_at,
                entry.ended_at,
                entry.duration_ms,
                context_json,
                entry.tag_id,
                entry.executor_type,
                entry.executor,
            ],
        )?;

        Ok(self.conn.last_insert_rowid())
    }

    /// Get an entry by ID
    #[cfg(test)]
    pub fn get_entry(&self, id: i64) -> DbResult<Option<Entry>> {
        let sql = format!(
            "SELECT {ENTRY_COLUMNS}
             FROM entries e
             LEFT JOIN sessions s ON e.session_id = s.id
             LEFT JOIN tags st ON s.tag_id = st.id
             LEFT JOIN tags et ON e.tag_id = et.id
             WHERE e.id = ?1"
        );
        let mut stmt = self.conn.prepare(&sql)?;
        let mut rows = stmt.query(params![id])?;

        if let Some(row) = rows.next()? {
            Ok(Some(entry_from_row(row, 10)?))
        } else {
            Ok(None)
        }
    }

    /// Get all entries for a session
    #[cfg(test)]
    pub fn get_entries_by_session(&self, session_id: &str) -> DbResult<Vec<Entry>> {
        let sql = format!(
            "SELECT {ENTRY_COLUMNS}
             FROM entries e
             LEFT JOIN sessions s ON e.session_id = s.id
             LEFT JOIN tags st ON s.tag_id = st.id
             LEFT JOIN tags et ON e.tag_id = et.id
             WHERE e.session_id = ?1 ORDER BY e.started_at DESC"
        );
        let mut stmt = self.conn.prepare(&sql)?;

        let entries = stmt
            .query_map(params![session_id], |row| entry_from_row(row, 10))?
            .collect::<Result<Vec<_>, _>>()?;

        Ok(entries)
    }

    /// Count all entries
    #[cfg(test)]
    pub fn count_entries(&self) -> DbResult<i64> {
        let count: i64 = self
            .conn
            .query_row("SELECT COUNT(*) FROM entries", [], |row| row.get(0))?;
        Ok(count)
    }

    /// Get entries with optional filters and field-specific search
    #[allow(clippy::cast_possible_wrap)]
    pub fn get_entries_filtered(
        &self,
        limit: usize,
        offset: usize,
        filter: &super::QueryFilter,
    ) -> DbResult<Vec<Entry>> {
        let mut fb = filter.to_filter_builder();

        let sql = format!(
            "SELECT {ENTRY_COLUMNS} {ENTRY_JOINS}{} ORDER BY e.started_at DESC LIMIT ? OFFSET ?",
            fb.build_where()
        );
        fb.push_param(Box::new(limit as i64));
        fb.push_param(Box::new(offset as i64));

        let mut stmt = self.conn.prepare(&sql)?;
        let entries = stmt
            .query_map(fb.params_refs().as_slice(), |row| entry_from_row(row, 10))?
            .collect::<Result<Vec<_>, _>>()?;

        Ok(entries)
    }

    /// Get entries in chronological order for replay.
    pub fn get_replay_entries(
        &self,
        session_id: Option<&str>,
        filter: &super::ReplayFilter,
    ) -> DbResult<Vec<Entry>> {
        let fb = FilterBuilder::new()
            .with_session(session_id)
            .with_date_range(filter.after, filter.before)
            .with_tag(filter.tag_id)
            .with_exit_code(filter.exit_code)
            .with_executor(filter.executor)
            .with_cwd(filter.cwd);

        let limit_clause = if filter.limit.is_some() {
            " LIMIT ?"
        } else {
            ""
        };

        let sql = format!(
            "SELECT {ENTRY_COLUMNS} {ENTRY_JOINS}{} ORDER BY e.started_at ASC{limit_clause}",
            fb.build_where()
        );

        let limit_val = filter.limit.map(|n| i64::try_from(n).unwrap_or(i64::MAX));
        let mut params = fb.params_refs();
        if let Some(ref val) = limit_val {
            params.push(val);
        }

        let mut stmt = self.conn.prepare(&sql)?;
        let entries = stmt
            .query_map(params.as_slice(), |row| entry_from_row(row, 10))?
            .collect::<Result<Vec<_>, _>>()?;

        Ok(entries)
    }

    /// Get entries with unique command deduplication using `QueryFilter`
    #[allow(clippy::cast_possible_wrap)]
    pub fn get_unique_entries_filtered(
        &self,
        limit: usize,
        offset: usize,
        filter: &super::QueryFilter,
        sort_alphabetically: bool,
    ) -> DbResult<Vec<(Entry, i64)>> {
        let mut fb = filter.to_filter_builder();

        let order = if sort_alphabetically {
            "e.command ASC"
        } else {
            "recent_start DESC"
        };

        // Use a subquery to deterministically select the most recent row per command.
        // The inner query groups by command to get max id and count; the outer query
        // joins back to get all columns from that specific row.
        let inner_where = fb.build_where();
        let sql = format!(
            "SELECT e.id, e.session_id, e.command, e.cwd, e.exit_code,
                e.started_at as recent_start, e.ended_at, e.duration_ms, e.context,
                COALESCE(et.name, st.name) as name,
                g.occurrence_count,
                e.tag_id, e.executor_type, e.executor
             FROM (
                SELECT MAX(e.id) as max_id, COUNT(*) as occurrence_count, e.command
                {ENTRY_JOINS}{inner_where} GROUP BY e.command
             ) g
             JOIN entries e ON e.id = g.max_id
             LEFT JOIN tags et ON e.tag_id = et.id
             LEFT JOIN sessions s ON e.session_id = s.id
             LEFT JOIN tags st ON s.tag_id = st.id
             ORDER BY {order} LIMIT ? OFFSET ?",
        );
        fb.push_param(Box::new(limit as i64));
        fb.push_param(Box::new(offset as i64));

        let mut stmt = self.conn.prepare(&sql)?;

        let results = stmt
            .query_map(fb.params_refs().as_slice(), |row| {
                let count: i64 = row.get(10)?;
                let entry = entry_from_row(row, 11)?;
                Ok((entry, count))
            })?
            .collect::<Result<Vec<_>, _>>()?;

        Ok(results)
    }

    /// Get entries ordered by recency without deduplication.
    /// Used by arrow-key navigation so that every invocation (including
    /// failed commands) is accessible. When `boost_cwd` is provided,
    /// same-directory entries sort before others at the same recency tier.
    #[allow(clippy::cast_possible_wrap)]
    pub fn get_recent_entries(
        &self,
        limit: usize,
        offset: usize,
        query: Option<&str>,
        prefix_match: bool,
        boost_cwd: Option<&str>,
    ) -> DbResult<Vec<Entry>> {
        let mut fb = FilterBuilder::new().with_query(query, prefix_match);

        let cwd_order = if boost_cwd.is_some() {
            "CASE WHEN e.cwd = ? THEN 0 ELSE 1 END, "
        } else {
            ""
        };

        let sql = format!(
            "SELECT {ENTRY_COLUMNS}
             {ENTRY_JOINS}{} ORDER BY {cwd_order}e.started_at DESC LIMIT ? OFFSET ?",
            fb.build_where()
        );

        if let Some(cwd) = boost_cwd {
            fb.push_param(Box::new(cwd.to_string()));
        }
        fb.push_param(Box::new(limit as i64));
        fb.push_param(Box::new(offset as i64));

        let mut stmt = self.conn.prepare(&sql)?;

        let results = stmt
            .query_map(fb.params_refs().as_slice(), |row| entry_from_row(row, 10))?
            .collect::<Result<Vec<_>, _>>()?;

        Ok(results)
    }

    /// Count unique entries matching `QueryFilter`
    pub fn count_unique_filtered(&self, filter: &super::QueryFilter) -> DbResult<i64> {
        let fb = filter.to_filter_builder();

        let sql = format!(
            "SELECT COUNT(DISTINCT command) FROM entries e
             JOIN sessions s ON e.session_id = s.id{}",
            fb.build_where()
        );

        let mut stmt = self.conn.prepare(&sql)?;
        let count: i64 = stmt.query_row(fb.params_refs().as_slice(), |row| row.get(0))?;
        Ok(count)
    }

    /// Delete entries matching a pattern (and optionally older than a timestamp)
    pub fn delete_entries(
        &self,
        pattern: &str,
        is_regex: bool,
        before_timestamp: Option<i64>,
    ) -> DbResult<usize> {
        if is_regex {
            // Validate regex before sending to SQLite
            regex::Regex::new(pattern)
                .map_err(|e| crate::db::DbError::Validation(e.to_string()))?;

            // Use SQLite REGEXP function — no need to load rows into memory
            let mut sql = String::from("DELETE FROM entries WHERE command REGEXP ?1");
            let mut param_values: Vec<Box<dyn rusqlite::ToSql>> = Vec::new();
            param_values.push(Box::new(pattern.to_string()));

            if let Some(ts) = before_timestamp {
                sql.push_str(" AND started_at < ?2");
                param_values.push(Box::new(ts));
            }

            let count = self
                .conn
                .execute(&sql, rusqlite::params_from_iter(param_values.iter()))?;
            Ok(count)
        } else {
            let escaped = super::escape_like(pattern);
            let mut sql = String::from("DELETE FROM entries WHERE command LIKE ?1 ESCAPE '\\'");
            let mut params: Vec<Box<dyn rusqlite::ToSql>> = Vec::new();
            params.push(Box::new(format!("%{escaped}%")));

            if let Some(ts) = before_timestamp {
                sql.push_str(" AND started_at < ?2");
                params.push(Box::new(ts));
            }

            let count = self
                .conn
                .execute(&sql, rusqlite::params_from_iter(params.iter()))?;
            Ok(count)
        }
    }

    /// Count preview of deletion (Dry Run)
    pub fn count_entries_by_pattern(
        &self,
        pattern: &str,
        is_regex: bool,
        before_timestamp: Option<i64>,
    ) -> DbResult<usize> {
        if is_regex {
            // Validate regex before sending to SQLite
            regex::Regex::new(pattern)
                .map_err(|e| crate::db::DbError::Validation(e.to_string()))?;

            // Use SQLite REGEXP function — no need to load rows into memory
            let mut sql = String::from("SELECT COUNT(*) FROM entries WHERE command REGEXP ?1");
            let mut param_values: Vec<Box<dyn rusqlite::ToSql>> = Vec::new();
            param_values.push(Box::new(pattern.to_string()));

            if let Some(ts) = before_timestamp {
                sql.push_str(" AND started_at < ?2");
                param_values.push(Box::new(ts));
            }

            let count: i64 = self.conn.query_row(
                &sql,
                rusqlite::params_from_iter(param_values.iter()),
                |row| row.get(0),
            )?;
            Ok(
                usize::try_from(count)
                    .map_err(|e| crate::db::DbError::Validation(e.to_string()))?,
            )
        } else {
            let escaped = super::escape_like(pattern);
            let mut sql =
                String::from("SELECT COUNT(*) FROM entries WHERE command LIKE ?1 ESCAPE '\\'");
            let mut params: Vec<Box<dyn rusqlite::ToSql>> = Vec::new();
            params.push(Box::new(format!("%{escaped}%")));

            if let Some(ts) = before_timestamp {
                sql.push_str(" AND started_at < ?2");
                params.push(Box::new(ts));
            }

            let count: i64 =
                self.conn
                    .query_row(&sql, rusqlite::params_from_iter(params.iter()), |row| {
                        row.get(0)
                    })?;
            Ok(
                usize::try_from(count)
                    .map_err(|e| crate::db::DbError::Validation(e.to_string()))?,
            )
        }
    }

    /// Export all entries with optional date filtering (no pagination)
    #[cfg(test)]
    pub fn export_entries(&self, after: Option<i64>, before: Option<i64>) -> DbResult<Vec<Entry>> {
        let filter = FilterBuilder::new().with_date_range(after, before);
        let where_clause = filter.build_where();
        let param_refs = filter.params_refs();

        let sql = format!(
            "SELECT {ENTRY_COLUMNS} {ENTRY_JOINS} {where_clause} ORDER BY e.started_at ASC"
        );
        let mut stmt = self.conn.prepare(&sql)?;
        let entries = stmt
            .query_map(rusqlite::params_from_iter(param_refs), |row| {
                entry_from_row(row, 10)
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(entries)
    }

    /// Stream entries for export, calling `f` for each row without collecting all into memory.
    pub fn stream_export_entries<F>(
        &self,
        after: Option<i64>,
        before: Option<i64>,
        mut f: F,
    ) -> Result<(), Box<dyn std::error::Error>>
    where
        F: FnMut(Entry) -> Result<(), Box<dyn std::error::Error>>,
    {
        let filter = FilterBuilder::new().with_date_range(after, before);
        let where_clause = filter.build_where();
        let param_refs = filter.params_refs();

        let sql = format!(
            "SELECT {ENTRY_COLUMNS} {ENTRY_JOINS} {where_clause} ORDER BY e.started_at ASC"
        );
        let mut stmt = self.conn.prepare(&sql)?;
        let mut rows = stmt.query(rusqlite::params_from_iter(param_refs))?;

        while let Some(row) = rows.next()? {
            let entry = entry_from_row(row, 10)?;
            f(entry)?;
        }

        Ok(())
    }

    /// Delete an entry by ID
    pub fn delete_entry(&self, id: i64) -> DbResult<()> {
        self.conn
            .execute("DELETE FROM entries WHERE id = ?1", params![id])?;
        Ok(())
    }

    /// Count entries matching a `QueryFilter`
    pub fn count_filtered(&self, filter: &super::QueryFilter) -> DbResult<i64> {
        let fb = filter.to_filter_builder();

        let sql = format!(
            "SELECT COUNT(*) FROM entries e LEFT JOIN sessions s ON e.session_id = s.id{}",
            fb.build_where()
        );

        let mut stmt = self.conn.prepare(&sql)?;
        let count: i64 = stmt.query_row(fb.params_refs().as_slice(), |row| row.get(0))?;
        Ok(count)
    }

    /// Count orphaned sessions (sessions with no remaining entries).
    pub fn count_orphaned_sessions(&self) -> DbResult<i64> {
        let count: i64 = self.conn.query_row(
            "SELECT COUNT(*) FROM sessions s WHERE NOT EXISTS (SELECT 1 FROM entries e WHERE e.session_id = s.id)",
            [],
            |row| row.get(0),
        )?;
        Ok(count)
    }

    /// Delete orphaned sessions. Returns count deleted.
    pub fn delete_orphaned_sessions(&self) -> DbResult<usize> {
        let count = self.conn.execute(
            "DELETE FROM sessions WHERE id NOT IN (SELECT DISTINCT session_id FROM entries)",
            [],
        )?;
        Ok(count)
    }

    /// Count orphaned notes (notes referencing non-existent entries).
    pub fn count_orphaned_notes(&self) -> DbResult<i64> {
        let count: i64 = self.conn.query_row(
            "SELECT COUNT(*) FROM notes n WHERE NOT EXISTS (SELECT 1 FROM entries e WHERE e.id = n.entry_id)",
            [],
            |row| row.get(0),
        )?;
        Ok(count)
    }

    /// Delete orphaned notes. Returns count deleted.
    pub fn delete_orphaned_notes(&self) -> DbResult<usize> {
        let count = self.conn.execute(
            "DELETE FROM notes WHERE entry_id NOT IN (SELECT id FROM entries)",
            [],
        )?;
        Ok(count)
    }

    /// List recent sessions with summary stats (only sessions that have entries).
    #[allow(clippy::cast_possible_wrap)]
    pub fn list_sessions(
        &self,
        after: Option<i64>,
        tag_id: Option<i64>,
        limit: usize,
    ) -> DbResult<Vec<SessionSummary>> {
        let mut fb = super::FilterBuilder::new()
            .with_session_created_after(after)
            .with_session_tag(tag_id);

        let where_clause = fb.build_where();

        let sql = format!(
            "SELECT s.id, s.hostname, s.created_at, COALESCE(t.name, '') as tag_name,
                    COUNT(e.id) as cmd_count,
                    SUM(CASE WHEN e.exit_code = 0 THEN 1 ELSE 0 END) as success_count,
                    MIN(e.started_at) as first_cmd, MAX(e.ended_at) as last_cmd
             FROM sessions s
             LEFT JOIN entries e ON e.session_id = s.id
             LEFT JOIN tags t ON s.tag_id = t.id
             {where_clause}
             GROUP BY s.id
             HAVING cmd_count > 0
             ORDER BY s.created_at DESC
             LIMIT ?"
        );
        fb.push_param(Box::new(limit as i64));

        let param_refs = fb.params_refs();
        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt.query_map(param_refs.as_slice(), |row| {
            let tag: String = row.get(3)?;
            Ok(SessionSummary {
                id: row.get(0)?,
                hostname: row.get(1)?,
                created_at: row.get(2)?,
                tag_name: if tag.is_empty() { None } else { Some(tag) },
                cmd_count: row.get(4)?,
                success_count: row.get(5)?,
                first_cmd_at: row.get(6)?,
                last_cmd_at: row.get(7)?,
            })
        })?;

        let mut sessions = Vec::new();
        for row in rows {
            sessions.push(row?);
        }
        Ok(sessions)
    }

    /// Find sessions matching a prefix. Returns matching session IDs.
    pub fn find_sessions_by_prefix(&self, prefix: &str) -> DbResult<Vec<String>> {
        let mut stmt = self.conn.prepare(
            "SELECT id FROM sessions WHERE id LIKE ?1 ESCAPE '\\' ORDER BY created_at DESC LIMIT 10",
        )?;
        let escaped = super::escape_like(prefix);
        let pattern = format!("{escaped}%");
        let rows = stmt.query_map(params![pattern], |row| row.get::<_, String>(0))?;
        let mut ids = Vec::new();
        for row in rows {
            ids.push(row?);
        }
        Ok(ids)
    }

    /// Run VACUUM to reclaim disk space.
    pub fn vacuum(&self) -> DbResult<()> {
        self.conn.execute_batch("VACUUM")?;
        Ok(())
    }
}
