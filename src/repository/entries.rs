use crate::db::DbResult;
use crate::models::Entry;
use rusqlite::params;

use super::{entry_from_row, FilterBuilder, Repository, ENTRY_COLUMNS, ENTRY_JOINS};

impl Repository {
    /// Insert a new entry
    pub fn insert_entry(&self, entry: &Entry) -> DbResult<i64> {
        let context_json = entry.context.as_ref().map(|c| {
            serde_json::to_string(c).unwrap_or_else(|e| {
                eprintln!("suvadu: failed to serialize entry context: {e}");
                String::new()
            })
        });

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

    /// Get entries with optional filters
    #[allow(clippy::too_many_arguments, clippy::cast_possible_wrap)]
    pub fn get_entries(
        &self,
        limit: usize,
        offset: usize,
        after: Option<i64>,
        before: Option<i64>,
        tag_id: Option<i64>,
        exit_code: Option<i32>,
        query: Option<&str>,
        prefix_match: bool,
        executor: Option<&str>,
        cwd: Option<&str>,
    ) -> DbResult<Vec<Entry>> {
        let mut fb = FilterBuilder::new()
            .with_date_range(after, before)
            .with_tag(tag_id)
            .with_exit_code(exit_code)
            .with_query(query, prefix_match)
            .with_executor(executor)
            .with_cwd(cwd);

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

    /// Get entries in chronological order for replay
    #[allow(clippy::too_many_arguments)]
    pub fn get_replay_entries(
        &self,
        session_id: Option<&str>,
        after: Option<i64>,
        before: Option<i64>,
        tag_id: Option<i64>,
        exit_code: Option<i32>,
        executor: Option<&str>,
        cwd: Option<&str>,
    ) -> DbResult<Vec<Entry>> {
        let fb = FilterBuilder::new()
            .with_session(session_id)
            .with_date_range(after, before)
            .with_tag(tag_id)
            .with_exit_code(exit_code)
            .with_executor(executor)
            .with_cwd(cwd);

        let sql = format!(
            "SELECT {ENTRY_COLUMNS} {ENTRY_JOINS}{} ORDER BY e.started_at ASC",
            fb.build_where()
        );

        let mut stmt = self.conn.prepare(&sql)?;
        let entries = stmt
            .query_map(fb.params_refs().as_slice(), |row| entry_from_row(row, 10))?
            .collect::<Result<Vec<_>, _>>()?;

        Ok(entries)
    }

    /// Get entries with unique command deduplication
    #[allow(clippy::too_many_arguments, clippy::cast_possible_wrap)]
    pub fn get_unique_entries(
        &self,
        limit: usize,
        offset: usize,
        after: Option<i64>,
        before: Option<i64>,
        tag_id: Option<i64>,
        exit_code: Option<i32>,
        query: Option<&str>,
        prefix_match: bool,
        sort_alphabetically: bool,
        executor: Option<&str>,
        cwd: Option<&str>,
    ) -> DbResult<Vec<(Entry, i64)>> {
        let mut fb = FilterBuilder::new()
            .with_date_range(after, before)
            .with_tag(tag_id)
            .with_exit_code(exit_code)
            .with_query(query, prefix_match)
            .with_executor(executor)
            .with_cwd(cwd);

        let order = if sort_alphabetically {
            "e.command ASC"
        } else {
            "recent_start DESC"
        };

        let sql = format!(
            "SELECT e.id, e.session_id, e.command, e.cwd, e.exit_code,
                MAX(e.started_at) as recent_start, e.ended_at, e.duration_ms, e.context,
                COALESCE(et.name, st.name) as name,
                COUNT(*) as occurrence_count,
                e.tag_id, e.executor_type, e.executor
             {ENTRY_JOINS}{} GROUP BY e.command ORDER BY {order} LIMIT ? OFFSET ?",
            fb.build_where()
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

    /// Count unique entries matching filters
    #[allow(clippy::too_many_arguments)]
    pub fn count_unique_entries(
        &self,
        after: Option<i64>,
        before: Option<i64>,
        tag_id: Option<i64>,
        exit_code: Option<i32>,
        query: Option<&str>,
        prefix_match: bool,
        executor: Option<&str>,
        cwd: Option<&str>,
    ) -> DbResult<i64> {
        let fb = FilterBuilder::new()
            .with_date_range(after, before)
            .with_tag(tag_id)
            .with_exit_code(exit_code)
            .with_query(query, prefix_match)
            .with_executor(executor)
            .with_cwd(cwd);

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
            let mut stmt = self
                .conn
                .prepare("SELECT id, command, started_at FROM entries")?;
            let regex = regex::Regex::new(pattern)
                .map_err(|e| crate::db::DbError::Validation(e.to_string()))?;

            let ids_to_delete: Vec<i64> = stmt
                .query_map([], |row| {
                    let id: i64 = row.get(0)?;
                    let cmd: String = row.get(1)?;
                    let started_at: i64 = row.get(2)?;
                    Ok((id, cmd, started_at))
                })?
                .filter_map(|r| match r {
                    Ok(v) => Some(v),
                    Err(e) => {
                        eprintln!("suvadu: skipping row during delete: {e}");
                        None
                    }
                })
                .filter(|(_, cmd, started_at)| {
                    let match_regex = regex.is_match(cmd);
                    let match_date = before_timestamp.is_none_or(|ts| *started_at < ts);
                    match_regex && match_date
                })
                .map(|(id, _, _)| id)
                .collect();

            if ids_to_delete.is_empty() {
                return Ok(0);
            }

            let mut total_deleted = 0;
            for chunk in ids_to_delete.chunks(900) {
                let placeholders = chunk.iter().map(|_| "?").collect::<Vec<_>>().join(",");
                let sql = format!("DELETE FROM entries WHERE id IN ({placeholders})");
                let params: Vec<&dyn rusqlite::ToSql> =
                    chunk.iter().map(|id| id as &dyn rusqlite::ToSql).collect();
                total_deleted += self.conn.execute(&sql, params.as_slice())?;
            }

            Ok(total_deleted)
        } else {
            let mut sql = String::from("DELETE FROM entries WHERE command LIKE ?1");
            let mut params: Vec<Box<dyn rusqlite::ToSql>> = Vec::new();
            params.push(Box::new(format!("%{pattern}%")));

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
            let mut stmt = self
                .conn
                .prepare("SELECT command, started_at FROM entries")?;
            let regex = regex::Regex::new(pattern)
                .map_err(|e| crate::db::DbError::Validation(e.to_string()))?;

            let count = stmt
                .query_map([], |row| {
                    let cmd: String = row.get(0)?;
                    let started_at: i64 = row.get(1)?;
                    Ok((cmd, started_at))
                })?
                .filter_map(|r| match r {
                    Ok(v) => Some(v),
                    Err(e) => {
                        eprintln!("suvadu: skipping row during count: {e}");
                        None
                    }
                })
                .filter(|(cmd, started_at)| {
                    let match_regex = regex.is_match(cmd);
                    let match_date = before_timestamp.is_none_or(|ts| *started_at < ts);
                    match_regex && match_date
                })
                .count();
            Ok(count)
        } else {
            let mut sql = String::from("SELECT COUNT(*) FROM entries WHERE command LIKE ?1");
            let mut params: Vec<Box<dyn rusqlite::ToSql>> = Vec::new();
            params.push(Box::new(format!("%{pattern}%")));

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

    /// Delete an entry by ID
    pub fn delete_entry(&self, id: i64) -> DbResult<()> {
        self.conn
            .execute("DELETE FROM entries WHERE id = ?1", params![id])?;
        Ok(())
    }

    /// Count entries matching filters
    #[allow(clippy::too_many_arguments)]
    pub fn count_filtered_entries(
        &self,
        after: Option<i64>,
        before: Option<i64>,
        tag_id: Option<i64>,
        exit_code: Option<i32>,
        query: Option<&str>,
        prefix_match: bool,
        executor: Option<&str>,
        cwd: Option<&str>,
    ) -> DbResult<i64> {
        let fb = FilterBuilder::new()
            .with_date_range(after, before)
            .with_tag(tag_id)
            .with_exit_code(exit_code)
            .with_query(query, prefix_match)
            .with_executor(executor)
            .with_cwd(cwd);

        let sql = format!(
            "SELECT COUNT(*) FROM entries e LEFT JOIN sessions s ON e.session_id = s.id{}",
            fb.build_where()
        );

        let mut stmt = self.conn.prepare(&sql)?;
        let count: i64 = stmt.query_row(fb.params_refs().as_slice(), |row| row.get(0))?;
        Ok(count)
    }
}
