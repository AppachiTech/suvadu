use crate::db::DbResult;
use crate::models::{Bookmark, Entry, Note, Session, Stats};
use rusqlite::{params, Connection};

/// Shared entry column list for SELECT queries
const ENTRY_COLUMNS: &str = "e.id, e.session_id, e.command, e.cwd, e.exit_code, e.started_at, e.ended_at, e.duration_ms, e.context, COALESCE(et.name, st.name) as tag_name, e.tag_id, e.executor_type, e.executor";

/// Shared FROM/JOIN clause for entry queries
const ENTRY_JOINS: &str = "FROM entries e
             JOIN sessions s ON e.session_id = s.id
             LEFT JOIN tags st ON s.tag_id = st.id
             LEFT JOIN tags et ON e.tag_id = et.id";

/// Maps a `SQLite` row to an `Entry`. `tag_id_col` is the column index where `tag_id` starts
/// (10 for standard queries, 11 for unique queries where COUNT(*) is at position 10).
fn entry_from_row(row: &rusqlite::Row, tag_id_col: usize) -> rusqlite::Result<Entry> {
    let context_str: Option<String> = row.get(8)?;
    let context = context_str.and_then(|s| serde_json::from_str(&s).ok());
    let tag_name: Option<String> = row.get(9)?;

    Ok(Entry {
        id: Some(row.get(0)?),
        session_id: row.get(1)?,
        command: row.get(2)?,
        cwd: row.get(3)?,
        exit_code: row.get(4)?,
        started_at: row.get(5)?,
        ended_at: row.get(6)?,
        duration_ms: row.get(7)?,
        context,
        tag_name,
        tag_id: row.get(tag_id_col)?,
        executor_type: row.get(tag_id_col + 1)?,
        executor: row.get(tag_id_col + 2)?,
    })
}

/// Builds WHERE clauses and collects parameters for filtered queries.
struct FilterBuilder {
    clauses: Vec<String>,
    params: Vec<Box<dyn rusqlite::ToSql>>,
}

impl FilterBuilder {
    fn new() -> Self {
        Self {
            clauses: Vec::new(),
            params: Vec::new(),
        }
    }

    fn with_date_range(mut self, after: Option<i64>, before: Option<i64>) -> Self {
        if let Some(start) = after {
            self.clauses.push("e.started_at >= ?".into());
            self.params.push(Box::new(start));
        }
        if let Some(end) = before {
            self.clauses.push("e.started_at <= ?".into());
            self.params.push(Box::new(end));
        }
        self
    }

    fn with_tag(mut self, tag_id: Option<i64>) -> Self {
        if let Some(tid) = tag_id {
            self.clauses.push("(s.tag_id = ? OR e.tag_id = ?)".into());
            self.params.push(Box::new(tid));
            self.params.push(Box::new(tid));
        }
        self
    }

    fn with_exit_code(mut self, exit_code: Option<i32>) -> Self {
        if let Some(code) = exit_code {
            self.clauses.push("e.exit_code = ?".into());
            self.params.push(Box::new(code));
        }
        self
    }

    fn with_query(mut self, query: Option<&str>, prefix_match: bool) -> Self {
        if let Some(q) = query {
            self.clauses.push("e.command LIKE ?".into());
            if prefix_match {
                self.params.push(Box::new(format!("{q}%")));
            } else {
                self.params.push(Box::new(format!("%{q}%")));
            }
        }
        self
    }

    fn with_cwd(mut self, cwd: Option<&str>) -> Self {
        if let Some(dir) = cwd {
            self.clauses.push("e.cwd = ?".into());
            self.params.push(Box::new(dir.to_string()));
        }
        self
    }

    fn with_session(mut self, session_id: Option<&str>) -> Self {
        if let Some(sid) = session_id {
            self.clauses.push("e.session_id = ?".into());
            self.params.push(Box::new(sid.to_string()));
        }
        self
    }

    fn with_executor(mut self, executor: Option<&str>) -> Self {
        if let Some(exec) = executor {
            self.clauses.push(
                "(e.executor_type LIKE ? OR e.executor LIKE ? OR (e.executor_type || '-' || e.executor) LIKE ?)".into(),
            );
            let pattern = format!("%{}%", exec.to_lowercase());
            self.params.push(Box::new(pattern.clone()));
            self.params.push(Box::new(pattern.clone()));
            self.params.push(Box::new(pattern));
        }
        self
    }

    fn build_where(&self) -> String {
        if self.clauses.is_empty() {
            " WHERE 1=1".into()
        } else {
            format!(" WHERE {}", self.clauses.join(" AND "))
        }
    }

    fn params_refs(&self) -> Vec<&dyn rusqlite::ToSql> {
        self.params
            .iter()
            .map(std::convert::AsRef::as_ref)
            .collect()
    }

    fn push_param(&mut self, val: Box<dyn rusqlite::ToSql>) {
        self.params.push(val);
    }
}

/// Repository for managing history entries and sessions
pub struct Repository {
    conn: Connection,
}

impl Repository {
    /// Create a new repository with the given connection
    pub fn new(conn: Connection) -> Self {
        Self { conn }
    }

    /// Insert a new session
    pub fn insert_session(&self, session: &Session) -> DbResult<()> {
        self.conn.execute(
            "INSERT INTO sessions (id, hostname, created_at) VALUES (?1, ?2, ?3)",
            params![session.id, session.hostname, session.created_at],
        )?;
        Ok(())
    }

    /// Get a session by ID
    pub fn get_session(&self, id: &str) -> DbResult<Option<Session>> {
        let mut stmt = self
            .conn
            .prepare("SELECT id, hostname, created_at, tag_id FROM sessions WHERE id = ?1")?;

        let mut rows = stmt.query(params![id])?;

        if let Some(row) = rows.next()? {
            Ok(Some(Session {
                id: row.get(0)?,
                hostname: row.get(1)?,
                created_at: row.get(2)?,
                tag_id: row.get(3)?,
            }))
        } else {
            Ok(None)
        }
    }

    /// Insert a new entry
    pub fn insert_entry(&self, entry: &Entry) -> DbResult<i64> {
        let context_json = entry
            .context
            .as_ref()
            .map(|c| serde_json::to_string(c).unwrap_or_default());

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
    #[allow(clippy::too_many_arguments)]
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
        fb.push_param(Box::new(limit));
        fb.push_param(Box::new(offset));

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

    /// Create a new tag
    pub fn create_tag(&self, name: &str, description: Option<&str>) -> DbResult<i64> {
        let count: i64 = self
            .conn
            .query_row("SELECT COUNT(*) FROM tags", [], |row| row.get(0))?;

        if count >= 20 {
            return Err(crate::db::DbError::Validation(
                "Maximum number of tags (20) reached".into(),
            ));
        }

        let name_lower = name.to_lowercase();

        self.conn.execute(
            "INSERT INTO tags (name, description) VALUES (?1, ?2)",
            params![name_lower, description],
        )?;

        Ok(self.conn.last_insert_rowid())
    }

    /// Get all tags
    pub fn get_tags(&self) -> DbResult<Vec<crate::models::Tag>> {
        let mut stmt = self
            .conn
            .prepare("SELECT id, name, description FROM tags ORDER BY name ASC")?;
        let tags = stmt
            .query_map([], |row| {
                Ok(crate::models::Tag {
                    id: row.get(0)?,
                    name: row.get(1)?,
                    description: row.get(2)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(tags)
    }

    /// Update a tag
    pub fn update_tag(&self, id: i64, name: &str, description: Option<&str>) -> DbResult<()> {
        let name_lower = name.to_lowercase();
        self.conn.execute(
            "UPDATE tags SET name = ?1, description = ?2 WHERE id = ?3",
            params![name_lower, description, id],
        )?;
        Ok(())
    }

    /// Associate a session with a tag (or clear if `tag_id` is None)
    pub fn tag_session(&self, session_id: &str, tag_id: Option<i64>) -> DbResult<()> {
        self.conn.execute(
            "UPDATE sessions SET tag_id = ?1 WHERE id = ?2",
            params![tag_id, session_id],
        )?;
        Ok(())
    }

    /// Get the tag name associated with a session
    pub fn get_tag_by_session(&self, session_id: &str) -> DbResult<Option<String>> {
        let mut stmt = self.conn.prepare(
            "SELECT t.name
             FROM sessions s
             JOIN tags t ON s.tag_id = t.id
             WHERE s.id = ?1",
        )?;

        let mut rows = stmt.query(params![session_id])?;

        if let Some(row) = rows.next()? {
            Ok(Some(row.get(0)?))
        } else {
            Ok(None)
        }
    }

    /// Get entries with unique command deduplication
    #[allow(clippy::too_many_arguments)]
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
        fb.push_param(Box::new(limit));
        fb.push_param(Box::new(offset));

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

    /// Get unique entries ordered by recency with optional CWD boost.
    /// Most recently executed command appears first. When `boost_cwd` is
    /// provided, same-directory entries sort before others at the same
    /// recency tier.
    #[allow(clippy::too_many_arguments)]
    pub fn get_frecent_entries(
        &self,
        limit: usize,
        offset: usize,
        query: Option<&str>,
        prefix_match: bool,
        boost_cwd: Option<&str>,
    ) -> DbResult<Vec<(Entry, i64)>> {
        let mut fb = FilterBuilder::new().with_query(query, prefix_match);

        let cwd_order = if boost_cwd.is_some() {
            "CASE WHEN e.cwd = ? THEN 0 ELSE 1 END, "
        } else {
            ""
        };

        let sql = format!(
            "SELECT e.id, e.session_id, e.command, e.cwd, e.exit_code,
                MAX(e.started_at) as recent_start, e.ended_at, e.duration_ms, e.context,
                COALESCE(et.name, st.name) as name,
                COUNT(*) as occurrence_count,
                e.tag_id, e.executor_type, e.executor
             {ENTRY_JOINS}{} GROUP BY e.command ORDER BY {cwd_order}recent_start DESC LIMIT ? OFFSET ?",
            fb.build_where()
        );

        if let Some(cwd) = boost_cwd {
            fb.push_param(Box::new(cwd.to_string()));
        }
        fb.push_param(Box::new(limit));
        fb.push_param(Box::new(offset));

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
                .filter_map(Result::ok)
                .filter(|(_, cmd, started_at)| {
                    let match_regex = regex.is_match(cmd);
                    let match_date = if let Some(ts) = before_timestamp {
                        *started_at < ts
                    } else {
                        true
                    };
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
                .filter_map(Result::ok)
                .filter(|(cmd, started_at)| {
                    let match_regex = regex.is_match(cmd);
                    let match_date = if let Some(ts) = before_timestamp {
                        *started_at < ts
                    } else {
                        true
                    };
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

    /// Get aggregated usage statistics
    #[allow(clippy::too_many_lines)]
    pub fn get_stats(&self, days: Option<usize>, top_n: usize) -> DbResult<Stats> {
        let time_filter = days.map(|d| {
            let now = chrono::Utc::now().timestamp_millis();
            now - (i64::try_from(d).unwrap_or(i64::MAX) * 24 * 60 * 60 * 1000)
        });
        let where_clause = match time_filter {
            Some(_) => " WHERE e.started_at >= ?1",
            None => "",
        };
        let bind = |stmt: &mut rusqlite::Statement| -> rusqlite::Result<()> {
            if let Some(ts) = time_filter {
                stmt.raw_bind_parameter(1, ts)?;
            }
            Ok(())
        };

        // Total commands
        let total_commands: i64 = {
            let sql = format!("SELECT COUNT(*) FROM entries e{where_clause}");
            let mut stmt = self.conn.prepare(&sql)?;
            bind(&mut stmt)?;
            let val = stmt
                .raw_query()
                .next()?
                .ok_or(crate::db::DbError::Validation(
                    "Expected row from aggregate query".into(),
                ))?
                .get(0)?;
            val
        };

        // Unique commands
        let unique_commands: i64 = {
            let sql = format!("SELECT COUNT(DISTINCT command) FROM entries e{where_clause}");
            let mut stmt = self.conn.prepare(&sql)?;
            bind(&mut stmt)?;
            let val = stmt
                .raw_query()
                .next()?
                .ok_or(crate::db::DbError::Validation(
                    "Expected row from aggregate query".into(),
                ))?
                .get(0)?;
            val
        };

        // Success / failure
        let success_count: i64 = {
            let extra = if where_clause.is_empty() {
                " WHERE e.exit_code = 0"
            } else {
                " AND e.exit_code = 0"
            };
            let sql = format!("SELECT COUNT(*) FROM entries e{where_clause}{extra}");
            let mut stmt = self.conn.prepare(&sql)?;
            bind(&mut stmt)?;
            let val = stmt
                .raw_query()
                .next()?
                .ok_or(crate::db::DbError::Validation(
                    "Expected row from aggregate query".into(),
                ))?
                .get(0)?;
            val
        };
        let failure_count = total_commands - success_count;

        // Average duration
        let avg_duration_ms: i64 = {
            let sql =
                format!("SELECT COALESCE(CAST(AVG(duration_ms) AS INTEGER), 0) FROM entries e{where_clause}");
            let mut stmt = self.conn.prepare(&sql)?;
            bind(&mut stmt)?;
            let val = stmt
                .raw_query()
                .next()?
                .ok_or(crate::db::DbError::Validation(
                    "Expected row from aggregate query".into(),
                ))?
                .get(0)?;
            val
        };

        // Top commands
        let top_commands: Vec<(String, i64)> = {
            let sql = format!(
                "SELECT command, COUNT(*) as cnt FROM entries e{where_clause} GROUP BY command ORDER BY cnt DESC LIMIT ?{}",
                if time_filter.is_some() { "2" } else { "1" }
            );
            let mut stmt = self.conn.prepare(&sql)?;
            bind(&mut stmt)?;
            let param_idx = if time_filter.is_some() { 2 } else { 1 };
            stmt.raw_bind_parameter(param_idx, i64::try_from(top_n).unwrap_or(i64::MAX))?;
            let mut rows = stmt.raw_query();
            let mut results = Vec::new();
            while let Some(row) = rows.next()? {
                results.push((row.get(0)?, row.get(1)?));
            }
            results
        };

        // Top directories
        let top_directories: Vec<(String, i64)> = {
            let sql = format!(
                "SELECT cwd, COUNT(*) as cnt FROM entries e{where_clause} GROUP BY cwd ORDER BY cnt DESC LIMIT ?{}",
                if time_filter.is_some() { "2" } else { "1" }
            );
            let mut stmt = self.conn.prepare(&sql)?;
            bind(&mut stmt)?;
            let param_idx = if time_filter.is_some() { 2 } else { 1 };
            stmt.raw_bind_parameter(param_idx, i64::try_from(top_n).unwrap_or(i64::MAX))?;
            let mut rows = stmt.raw_query();
            let mut results = Vec::new();
            while let Some(row) = rows.next()? {
                results.push((row.get(0)?, row.get(1)?));
            }
            results
        };

        // Hourly distribution
        let hourly_distribution: Vec<(u32, i64)> = {
            let sql = format!(
                "SELECT CAST(strftime('%H', datetime(e.started_at/1000, 'unixepoch', 'localtime')) AS INTEGER) as hour, \
                 COUNT(*) as cnt FROM entries e{where_clause} GROUP BY hour ORDER BY hour"
            );
            let mut stmt = self.conn.prepare(&sql)?;
            bind(&mut stmt)?;
            let mut rows = stmt.raw_query();
            let mut results = Vec::new();
            while let Some(row) = rows.next()? {
                if let Some(h) = row.get::<_, Option<i64>>(0)? {
                    let hour = u32::try_from(h).unwrap_or(0);
                    results.push((hour, row.get(1)?));
                }
            }
            results
        };

        // Executor breakdown
        let executor_breakdown: Vec<(String, i64)> = {
            let sql = format!(
                "SELECT COALESCE(e.executor_type, 'human') as exec_type, COUNT(*) as cnt \
                 FROM entries e{where_clause} GROUP BY exec_type ORDER BY cnt DESC"
            );
            let mut stmt = self.conn.prepare(&sql)?;
            bind(&mut stmt)?;
            let mut rows = stmt.raw_query();
            let mut results = Vec::new();
            while let Some(row) = rows.next()? {
                results.push((row.get(0)?, row.get(1)?));
            }
            results
        };

        Ok(Stats {
            total_commands,
            unique_commands,
            success_count,
            failure_count,
            avg_duration_ms,
            top_commands,
            top_directories,
            hourly_distribution,
            executor_breakdown,
            period_days: days,
        })
    }

    /// Get daily command counts for the heatmap and trend chart.
    /// Returns `(date_string, day_of_week 0=Sun..6=Sat, count)`.
    pub fn get_daily_activity(&self, days: usize) -> DbResult<Vec<(String, u32, i64)>> {
        let now = chrono::Utc::now().timestamp_millis();
        let since = now - (i64::try_from(days).unwrap_or(i64::MAX) * 24 * 60 * 60 * 1000);
        let sql = "SELECT \
                date(e.started_at/1000, 'unixepoch', 'localtime') as day, \
                CAST(strftime('%w', datetime(e.started_at/1000, 'unixepoch', 'localtime')) AS INTEGER) as dow, \
                COUNT(*) as cnt \
            FROM entries e \
            WHERE e.started_at >= ?1 \
            GROUP BY day \
            ORDER BY day ASC";
        let mut stmt = self.conn.prepare(sql)?;
        let mut rows = stmt.query(rusqlite::params![since])?;
        let mut results = Vec::new();
        while let Some(row) = rows.next()? {
            let day: String = row.get(0)?;
            let dow: u32 = row
                .get::<_, i64>(1)
                .map(|v| u32::try_from(v).unwrap_or(0))?;
            let cnt: i64 = row.get(2)?;
            results.push((day, dow, cnt));
        }
        Ok(results)
    }

    // ── Bookmarks ──────────────────────────────────────────────

    /// Add a bookmark (INSERT OR REPLACE to handle duplicates)
    pub fn add_bookmark(&self, command: &str, label: Option<&str>) -> DbResult<i64> {
        let now = chrono::Utc::now().timestamp_millis();
        self.conn.execute(
            "INSERT OR REPLACE INTO bookmarks (command, label, created_at) VALUES (?1, ?2, ?3)",
            params![command, label, now],
        )?;
        Ok(self.conn.last_insert_rowid())
    }

    /// Remove a bookmark by command text
    pub fn remove_bookmark(&self, command: &str) -> DbResult<bool> {
        let count = self
            .conn
            .execute("DELETE FROM bookmarks WHERE command = ?1", params![command])?;
        Ok(count > 0)
    }

    /// List all bookmarks ordered by most recent first
    pub fn list_bookmarks(&self) -> DbResult<Vec<Bookmark>> {
        let mut stmt = self
            .conn
            .prepare("SELECT id, command, label, created_at FROM bookmarks ORDER BY id DESC")?;
        let rows = stmt.query_map([], |row| {
            Ok(Bookmark {
                id: row.get(0)?,
                command: row.get(1)?,
                label: row.get(2)?,
                created_at: row.get(3)?,
            })
        })?;
        let mut bookmarks = Vec::new();
        for row in rows {
            bookmarks.push(row?);
        }
        Ok(bookmarks)
    }

    /// Get all bookmarked command strings as a set
    pub fn get_bookmarked_commands(&self) -> DbResult<std::collections::HashSet<String>> {
        let mut stmt = self.conn.prepare("SELECT command FROM bookmarks")?;
        let rows = stmt.query_map([], |row| row.get::<_, String>(0))?;
        let mut set = std::collections::HashSet::new();
        for row in rows {
            set.insert(row?);
        }
        Ok(set)
    }

    // ── Notes ──────────────────────────────────────────────────

    /// Upsert a note for a history entry
    pub fn upsert_note(&self, entry_id: i64, note: &str) -> DbResult<()> {
        let now = chrono::Utc::now().timestamp_millis();
        self.conn.execute(
            "INSERT INTO notes (entry_id, note, created_at, updated_at) VALUES (?1, ?2, ?3, ?3)
             ON CONFLICT(entry_id) DO UPDATE SET note = excluded.note, updated_at = excluded.updated_at",
            params![entry_id, note, now],
        )?;
        Ok(())
    }

    /// Get a note for a history entry
    pub fn get_note(&self, entry_id: i64) -> DbResult<Option<Note>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, entry_id, note, created_at, updated_at FROM notes WHERE entry_id = ?1",
        )?;
        let mut rows = stmt.query_map(params![entry_id], |row| {
            Ok(Note {
                id: row.get(0)?,
                entry_id: row.get(1)?,
                content: row.get(2)?,
                created_at: row.get(3)?,
                updated_at: row.get(4)?,
            })
        })?;
        match rows.next() {
            Some(Ok(note)) => Ok(Some(note)),
            Some(Err(e)) => Err(e.into()),
            None => Ok(None),
        }
    }

    /// Delete a note for a history entry
    pub fn delete_note(&self, entry_id: i64) -> DbResult<bool> {
        let count = self
            .conn
            .execute("DELETE FROM notes WHERE entry_id = ?1", params![entry_id])?;
        Ok(count > 0)
    }

    /// Begin a transaction (for batch operations)
    pub fn begin_transaction(&self) -> DbResult<()> {
        self.conn.execute_batch("BEGIN")?;
        Ok(())
    }

    /// Commit a transaction
    pub fn commit(&self) -> DbResult<()> {
        self.conn.execute_batch("COMMIT")?;
        Ok(())
    }

    /// Get all (command, started-at-seconds) pairs for dedup during import.
    /// Returns `started_at` / 1000 so zsh-history second-precision
    /// timestamps can be compared directly.
    pub fn get_existing_command_timestamps(
        &self,
    ) -> DbResult<std::collections::HashSet<(String, i64)>> {
        let mut stmt = self
            .conn
            .prepare("SELECT command, started_at / 1000 FROM entries")?;
        let rows = stmt.query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
        })?;
        let mut set = std::collections::HashSet::new();
        for row in rows {
            set.insert(row?);
        }
        Ok(set)
    }

    /// Get all entry IDs that have notes
    pub fn get_noted_entry_ids(&self) -> DbResult<std::collections::HashSet<i64>> {
        let mut stmt = self.conn.prepare("SELECT entry_id FROM notes")?;
        let rows = stmt.query_map([], |row| row.get::<_, i64>(0))?;
        let mut set = std::collections::HashSet::new();
        for row in rows {
            set.insert(row?);
        }
        Ok(set)
    }

    /// Get frequently-used commands for alias suggestion.
    /// Returns `(command, count, dir_count)` tuples filtered by minimum length and count.
    /// Results are ranked by `count * min(dir_count, 5)` — commands used across many
    /// directories are boosted as better alias candidates.
    pub fn get_frequent_commands(
        &self,
        days: Option<usize>,
        min_count: usize,
        min_length: usize,
        limit: usize,
    ) -> DbResult<Vec<(String, i64, i64)>> {
        #[allow(clippy::cast_possible_wrap)]
        let time_filter = days.map(|d| chrono::Utc::now().timestamp() - (d as i64 * 86400));

        let where_clause = if time_filter.is_some() {
            " WHERE LENGTH(e.command) >= ?1 AND e.started_at >= ?2"
        } else {
            " WHERE LENGTH(e.command) >= ?1"
        };

        let having_param = if time_filter.is_some() { "?3" } else { "?2" };
        let limit_param = if time_filter.is_some() { "?4" } else { "?3" };

        let sql = format!(
            "SELECT e.command, COUNT(*) as cnt, COUNT(DISTINCT e.cwd) as dir_cnt \
             FROM entries e{where_clause} \
             GROUP BY e.command HAVING cnt >= {having_param} \
             ORDER BY (cnt * MIN(dir_cnt, 5)) DESC LIMIT {limit_param}"
        );

        let mut stmt = self.conn.prepare(&sql)?;
        let min_len_i64 = i64::try_from(min_length).unwrap_or(i64::MAX);
        let min_cnt_i64 = i64::try_from(min_count).unwrap_or(i64::MAX);
        let limit_i64 = i64::try_from(limit).unwrap_or(i64::MAX);

        stmt.raw_bind_parameter(1, min_len_i64)?;
        if let Some(ts) = time_filter {
            stmt.raw_bind_parameter(2, ts)?;
            stmt.raw_bind_parameter(3, min_cnt_i64)?;
            stmt.raw_bind_parameter(4, limit_i64)?;
        } else {
            stmt.raw_bind_parameter(2, min_cnt_i64)?;
            stmt.raw_bind_parameter(3, limit_i64)?;
        }

        let mut rows = stmt.raw_query();
        let mut results = Vec::new();
        while let Some(row) = rows.next()? {
            results.push((row.get(0)?, row.get(1)?, row.get(2)?));
        }
        Ok(results)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::init_db;
    use crate::models::Session;
    use std::collections::HashMap;
    use tempfile::TempDir;

    fn setup_test_db() -> (TempDir, Repository) {
        let temp_dir = TempDir::new().unwrap();
        let db_path = temp_dir.path().join("test.db");
        let conn = init_db(&db_path).unwrap();
        let repo = Repository::new(conn);
        (temp_dir, repo)
    }

    #[test]
    fn test_insert_and_get_session() {
        let (_temp, repo) = setup_test_db();

        let session = Session::new("test-host".to_string(), 1000);
        repo.insert_session(&session).unwrap();

        let retrieved = repo.get_session(&session.id).unwrap();
        assert!(retrieved.is_some());

        let retrieved = retrieved.unwrap();
        assert_eq!(retrieved.id, session.id);
        assert_eq!(retrieved.hostname, "test-host");
        assert_eq!(retrieved.created_at, 1000);
    }

    #[test]
    fn test_insert_and_get_entry() {
        let (_temp, repo) = setup_test_db();

        let session = Session::new("test-host".to_string(), 1000);
        repo.insert_session(&session).unwrap();

        let entry = Entry::new(
            session.id.clone(),
            "ls -la".to_string(),
            "/home/user".to_string(),
            Some(0),
            1000,
            1050,
        );

        let entry_id = repo.insert_entry(&entry).unwrap();
        assert!(entry_id > 0);

        let retrieved = repo.get_entry(entry_id).unwrap();
        assert!(retrieved.is_some());

        let retrieved = retrieved.unwrap();
        assert_eq!(retrieved.command, "ls -la");
        assert_eq!(retrieved.exit_code, Some(0));
        assert_eq!(retrieved.duration_ms, 50);
    }

    #[test]
    fn test_entry_with_context() {
        let (_temp, repo) = setup_test_db();

        let session = Session::new("test-host".to_string(), 1000);
        repo.insert_session(&session).unwrap();

        let mut context = HashMap::new();
        context.insert("shell".to_string(), "zsh".to_string());
        context.insert("user".to_string(), "testuser".to_string());

        let mut entry = Entry::new(
            session.id.clone(),
            "echo test".to_string(),
            "/tmp".to_string(),
            Some(0),
            2000,
            2010,
        );
        entry.context = Some(context);

        let entry_id = repo.insert_entry(&entry).unwrap();

        let retrieved = repo.get_entry(entry_id).unwrap().unwrap();
        assert!(retrieved.context.is_some());

        let ctx = retrieved.context.unwrap();
        assert_eq!(ctx.get("shell").unwrap(), "zsh");
        assert_eq!(ctx.get("user").unwrap(), "testuser");
    }

    #[test]
    fn test_get_entries_by_session() {
        let (_temp, repo) = setup_test_db();

        let session = Session::new("test-host".to_string(), 1000);
        repo.insert_session(&session).unwrap();

        for i in 0..5 {
            let entry = Entry::new(
                session.id.clone(),
                format!("command_{i}"),
                "/tmp".to_string(),
                Some(0),
                1000 + i * 100,
                1050 + i * 100,
            );
            repo.insert_entry(&entry).unwrap();
        }

        let entries = repo.get_entries_by_session(&session.id).unwrap();
        assert_eq!(entries.len(), 5);

        assert_eq!(entries[0].command, "command_4");
        assert_eq!(entries[4].command, "command_0");
    }

    #[test]
    fn test_count_entries() {
        let (_temp, repo) = setup_test_db();

        let session = Session::new("test-host".to_string(), 1000);
        repo.insert_session(&session).unwrap();

        assert_eq!(repo.count_entries().unwrap(), 0);

        let entry = Entry::new(
            session.id.clone(),
            "test".to_string(),
            "/tmp".to_string(),
            Some(0),
            1000,
            1050,
        );
        repo.insert_entry(&entry).unwrap();

        assert_eq!(repo.count_entries().unwrap(), 1);
    }

    #[test]
    fn test_tag_limits_and_constraints() {
        {
            let (_temp, repo) = setup_test_db();
            for i in 0..20 {
                repo.create_tag(&format!("tag_{i}"), None).unwrap();
            }

            let err = repo.create_tag("tag_overflow", None);
            assert!(err.is_err());
            match err.unwrap_err() {
                crate::db::DbError::Validation(msg) => assert!(msg.contains("Maximum number")),
                other => panic!("Expected Validation error, got {:?}", other),
            }
        }

        {
            let (_temp, repo) = setup_test_db();
            let _id = repo.create_tag("UpPeR", None).unwrap();
            let tags = repo.get_tags().unwrap();
            assert_eq!(tags[0].name, "upper");

            let err = repo.create_tag("upper", None).unwrap_err();
            assert!(matches!(err, crate::db::DbError::Sqlite(_)));
        }
    }

    #[test]
    fn test_entries_filtering_by_tag() {
        let (_temp, repo) = setup_test_db();

        let work_tag = repo.create_tag("work", None).unwrap();
        let session_work = Session::new("host".to_string(), 100);
        repo.insert_session(&session_work).unwrap();
        repo.tag_session(&session_work.id, Some(work_tag)).unwrap();

        let entry_work = Entry::new(
            session_work.id.clone(),
            "git commit".to_string(),
            "/work".to_string(),
            None,
            1000,
            1010,
        );
        repo.insert_entry(&entry_work).unwrap();

        let personal_tag = repo.create_tag("personal", None).unwrap();
        let session_personal = Session::new("host".to_string(), 200);
        repo.insert_session(&session_personal).unwrap();
        repo.tag_session(&session_personal.id, Some(personal_tag))
            .unwrap();

        let entry_personal = Entry::new(
            session_personal.id.clone(),
            "steam".to_string(),
            "/games".to_string(),
            None,
            2000,
            2010,
        );
        repo.insert_entry(&entry_personal).unwrap();

        let session_untagged = Session::new("host".to_string(), 300);
        repo.insert_session(&session_untagged).unwrap();
        let entry_untagged = Entry::new(
            session_untagged.id.clone(),
            "ls".to_string(),
            "/".to_string(),
            None,
            3000,
            3010,
        );
        repo.insert_entry(&entry_untagged).unwrap();

        let work_entries = repo
            .get_entries(
                10,
                0,
                None,
                None,
                Some(work_tag),
                None,
                None,
                false,
                None,
                None,
            )
            .unwrap();
        assert_eq!(work_entries.len(), 1);
        assert_eq!(work_entries[0].command, "git commit");

        let work_count = repo
            .count_filtered_entries(None, None, Some(work_tag), None, None, false, None, None)
            .unwrap();
        assert_eq!(work_count, 1);

        let personal_entries = repo
            .get_entries(
                10,
                0,
                None,
                None,
                Some(personal_tag),
                None,
                None,
                false,
                None,
                None,
            )
            .unwrap();
        assert_eq!(personal_entries.len(), 1);
        assert_eq!(personal_entries[0].command, "steam");

        let all = repo
            .get_entries(10, 0, None, None, None, None, None, false, None, None)
            .unwrap();
        assert_eq!(all.len(), 3);
    }

    #[test]
    fn test_unique_entries_filtering_by_tag() {
        let (_temp, repo) = setup_test_db();
        let work_tag = repo.create_tag("work", None).unwrap();

        let session_work = Session::new("host".to_string(), 100);
        repo.insert_session(&session_work).unwrap();
        repo.tag_session(&session_work.id, Some(work_tag)).unwrap();

        repo.insert_entry(&Entry::new(
            session_work.id.clone(),
            "ls".into(),
            "/".into(),
            None,
            100,
            200,
        ))
        .unwrap();
        repo.insert_entry(&Entry::new(
            session_work.id.clone(),
            "ls".into(),
            "/".into(),
            None,
            110,
            210,
        ))
        .unwrap();
        repo.insert_entry(&Entry::new(
            session_work.id.clone(),
            "make".into(),
            "/".into(),
            None,
            120,
            220,
        ))
        .unwrap();

        let session_other = Session::new("host".to_string(), 200);
        repo.insert_session(&session_other).unwrap();
        repo.insert_entry(&Entry::new(
            session_other.id.clone(),
            "ls".into(),
            "/".into(),
            None,
            300,
            400,
        ))
        .unwrap();

        let unique_work = repo
            .get_unique_entries(
                10,
                0,
                None,
                None,
                Some(work_tag),
                None,
                None,
                false,
                false,
                None,
                None,
            )
            .unwrap();
        assert_eq!(unique_work.len(), 2);

        let ls_entry = unique_work.iter().find(|(e, _)| e.command == "ls").unwrap();
        assert_eq!(ls_entry.1, 2);

        let unique_count = repo
            .count_unique_entries(None, None, Some(work_tag), None, None, false, None, None)
            .unwrap();
        assert_eq!(unique_count, 2);

        let unique_global = repo
            .get_unique_entries(
                10, 0, None, None, None, None, None, false, false, None, None,
            )
            .unwrap();
        assert_eq!(unique_global.len(), 2);
        let ls_global = unique_global
            .iter()
            .find(|(e, _)| e.command == "ls")
            .unwrap();
        assert_eq!(ls_global.1, 3);
    }

    #[test]
    fn test_tag_lifecycle() {
        let (_temp, repo) = setup_test_db();

        let id = repo.create_tag("work", Some("Work stuff")).unwrap();
        let tags = repo.get_tags().unwrap();
        assert_eq!(tags.len(), 1);
        assert_eq!(tags[0].name, "work");

        let _id2 = repo.create_tag("Personal", None).unwrap();
        let tags = repo.get_tags().unwrap();
        assert_eq!(tags.len(), 2);
        assert_eq!(tags[0].name, "personal");
        assert_eq!(tags[1].name, "work");

        let err = repo.create_tag("WORK", None);
        assert!(err.is_err());

        repo.update_tag(id, "work_updated", None).unwrap();
        let tags = repo.get_tags().unwrap();
        assert_eq!(tags[1].name, "work_updated");

        let session = Session::new("host".to_string(), 100);
        repo.insert_session(&session).unwrap();

        repo.tag_session(&session.id, Some(id)).unwrap();
        let s = repo.get_session(&session.id).unwrap().unwrap();
        assert_eq!(s.tag_id, Some(id));

        repo.tag_session(&session.id, None).unwrap();
        let s = repo.get_session(&session.id).unwrap().unwrap();
        assert_eq!(s.tag_id, None);
    }

    #[test]
    fn test_unique_entries_query() {
        let (_temp, repo) = setup_test_db();

        let session = Session::new("test-host".to_string(), 1000);
        repo.insert_session(&session).unwrap();

        repo.insert_entry(&Entry::new(
            session.id.clone(),
            "ls".to_string(),
            "/tmp".to_string(),
            None,
            1000,
            1010,
        ))
        .unwrap();

        repo.insert_entry(&Entry::new(
            session.id.clone(),
            "ls".to_string(),
            "/tmp".to_string(),
            None,
            2000,
            2010,
        ))
        .unwrap();

        repo.insert_entry(&Entry::new(
            session.id.clone(),
            "ls".to_string(),
            "/tmp".to_string(),
            None,
            3000,
            3010,
        ))
        .unwrap();

        let entries = repo
            .get_unique_entries(
                10, 0, None, None, None, None, None, false, false, None, None,
            )
            .unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].0.command, "ls");
        assert_eq!(entries[0].0.started_at, 3000);
        assert_eq!(entries[0].1, 3);
    }

    #[test]
    fn test_unique_entries_pagination_and_query() {
        let (_temp, repo) = setup_test_db();
        let session = Session::new("host".to_string(), 100);
        repo.insert_session(&session).unwrap();

        let cmds = vec![
            ("git commit", 1000),
            ("git status", 2000),
            ("git commit", 3000),
            ("cargo build", 4000),
        ];

        for (cmd, time) in cmds {
            repo.insert_entry(&Entry::new(
                session.id.clone(),
                cmd.to_string(),
                "/".into(),
                None,
                time,
                time + 10,
            ))
            .unwrap();
        }

        let unique_git = repo
            .get_unique_entries(
                10,
                0,
                None,
                None,
                None,
                None,
                Some("git"),
                false,
                false,
                None,
                None,
            )
            .unwrap();
        assert_eq!(unique_git.len(), 2);
        assert_eq!(unique_git[0].0.command, "git commit");
        assert_eq!(unique_git[1].0.command, "git status");

        let page1 = repo
            .get_unique_entries(
                1,
                0,
                None,
                None,
                None,
                None,
                Some("git"),
                false,
                false,
                None,
                None,
            )
            .unwrap();
        assert_eq!(page1.len(), 1);
        assert_eq!(page1[0].0.command, "git commit");

        let page2 = repo
            .get_unique_entries(
                1,
                1,
                None,
                None,
                None,
                None,
                Some("git"),
                false,
                false,
                None,
                None,
            )
            .unwrap();
        assert_eq!(page2.len(), 1);
        assert_eq!(page2[0].0.command, "git status");

        let page3 = repo
            .get_unique_entries(
                1,
                2,
                None,
                None,
                None,
                None,
                Some("git"),
                false,
                false,
                None,
                None,
            )
            .unwrap();
        assert_eq!(page3.len(), 0);
    }

    #[test]
    fn test_unique_entries_recency_priority() {
        let (_temp, repo) = setup_test_db();
        let session = Session::new("host".to_string(), 100);
        repo.insert_session(&session).unwrap();

        repo.insert_entry(&Entry::new(
            session.id.clone(),
            "cmd_A".into(),
            "/".into(),
            None,
            1000,
            1010,
        ))
        .unwrap();

        repo.insert_entry(&Entry::new(
            session.id.clone(),
            "cmd_B".into(),
            "/".into(),
            None,
            2000,
            2010,
        ))
        .unwrap();

        repo.insert_entry(&Entry::new(
            session.id.clone(),
            "cmd_C".into(),
            "/".into(),
            None,
            3000,
            3010,
        ))
        .unwrap();

        let page1 = repo
            .get_unique_entries(1, 0, None, None, None, None, None, false, false, None, None)
            .unwrap();
        assert_eq!(page1.len(), 1);
        assert_eq!(page1[0].0.command, "cmd_C");

        let page2 = repo
            .get_unique_entries(1, 1, None, None, None, None, None, false, false, None, None)
            .unwrap();
        assert_eq!(page2.len(), 1);
        assert_eq!(page2[0].0.command, "cmd_B");

        let page3 = repo
            .get_unique_entries(1, 2, None, None, None, None, None, false, false, None, None)
            .unwrap();
        assert_eq!(page3.len(), 1);
        assert_eq!(page3[0].0.command, "cmd_A");
    }

    #[test]
    fn test_unique_entries_reexecution() {
        let (_temp, repo) = setup_test_db();
        let session = Session::new("host".to_string(), 100);
        repo.insert_session(&session).unwrap();

        repo.insert_entry(&Entry::new(
            session.id.clone(),
            "cmd_A".into(),
            "/".into(),
            None,
            1000,
            1010,
        ))
        .unwrap();

        repo.insert_entry(&Entry::new(
            session.id.clone(),
            "cmd_B".into(),
            "/".into(),
            None,
            2000,
            2010,
        ))
        .unwrap();

        repo.insert_entry(&Entry::new(
            session.id.clone(),
            "cmd_A".into(),
            "/".into(),
            None,
            3000,
            3010,
        ))
        .unwrap();

        let page1 = repo
            .get_unique_entries(1, 0, None, None, None, None, None, false, false, None, None)
            .unwrap();
        assert_eq!(page1.len(), 1);
        assert_eq!(page1[0].0.command, "cmd_A");

        let page2 = repo
            .get_unique_entries(1, 1, None, None, None, None, None, false, false, None, None)
            .unwrap();
        assert_eq!(page2.len(), 1);
        assert_eq!(page2[0].0.command, "cmd_B");
    }

    #[test]
    fn test_executor_tracking() {
        let (_temp, repo) = setup_test_db();

        let session = Session::new("test-host".to_string(), 1000);
        repo.insert_session(&session).unwrap();

        let mut entry = Entry::new(
            session.id.clone(),
            "cargo build".to_string(),
            "/home/user/project".to_string(),
            Some(0),
            1000,
            2000,
        );
        entry.executor_type = Some("human".to_string());
        entry.executor = Some("terminal".to_string());

        let entry_id = repo.insert_entry(&entry).unwrap();

        let retrieved = repo.get_entry(entry_id).unwrap().unwrap();
        assert_eq!(retrieved.executor_type, Some("human".to_string()));
        assert_eq!(retrieved.executor, Some("terminal".to_string()));
    }

    #[test]
    fn test_executor_types() {
        let (_temp, repo) = setup_test_db();

        let session = Session::new("test-host".to_string(), 1000);
        repo.insert_session(&session).unwrap();

        let executors = vec![
            ("human", "terminal"),
            ("ide", "vscode"),
            ("bot", "antigravity"),
            ("ci", "github-actions"),
        ];

        for (exec_type, exec_name) in executors {
            let mut entry = Entry::new(
                session.id.clone(),
                format!("test command for {}", exec_type),
                "/tmp".to_string(),
                Some(0),
                1000,
                2000,
            );
            entry.executor_type = Some(exec_type.to_string());
            entry.executor = Some(exec_name.to_string());

            let entry_id = repo.insert_entry(&entry).unwrap();
            let retrieved = repo.get_entry(entry_id).unwrap().unwrap();

            assert_eq!(retrieved.executor_type, Some(exec_type.to_string()));
            assert_eq!(retrieved.executor, Some(exec_name.to_string()));
        }
    }

    #[test]
    fn test_executor_null_values() {
        let (_temp, repo) = setup_test_db();

        let session = Session::new("test-host".to_string(), 1000);
        repo.insert_session(&session).unwrap();

        let entry = Entry::new(
            session.id.clone(),
            "old command".to_string(),
            "/tmp".to_string(),
            Some(0),
            1000,
            2000,
        );

        let entry_id = repo.insert_entry(&entry).unwrap();
        let retrieved = repo.get_entry(entry_id).unwrap().unwrap();

        assert_eq!(retrieved.executor_type, None);
        assert_eq!(retrieved.executor, None);
    }

    #[test]
    fn test_executor_filter_in_count() {
        let (_temp, repo) = setup_test_db();

        let session = Session::new("test-host".to_string(), 1000);
        repo.insert_session(&session).unwrap();

        let mut entry1 = Entry::new(
            session.id.clone(),
            "ls".to_string(),
            "/tmp".to_string(),
            Some(0),
            1000,
            2000,
        );
        entry1.executor_type = Some("human".to_string());
        entry1.executor = Some("terminal".to_string());
        repo.insert_entry(&entry1).unwrap();

        let mut entry2 = Entry::new(
            session.id.clone(),
            "git status".to_string(),
            "/tmp".to_string(),
            Some(0),
            2000,
            3000,
        );
        entry2.executor_type = Some("bot".to_string());
        entry2.executor = Some("antigravity".to_string());
        repo.insert_entry(&entry2).unwrap();

        // Count all
        let total = repo
            .count_filtered_entries(None, None, None, None, None, false, None, None)
            .unwrap();
        assert_eq!(total, 2);

        // Count only human
        let human_count = repo
            .count_filtered_entries(None, None, None, None, None, false, Some("human"), None)
            .unwrap();
        assert_eq!(human_count, 1);

        // Count only bot
        let bot_count = repo
            .count_filtered_entries(None, None, None, None, None, false, Some("bot"), None)
            .unwrap();
        assert_eq!(bot_count, 1);
    }

    #[test]
    fn test_stats_empty_db() {
        let (_temp, repo) = setup_test_db();
        let stats = repo.get_stats(None, 10).unwrap();
        assert_eq!(stats.total_commands, 0);
        assert_eq!(stats.unique_commands, 0);
        assert_eq!(stats.success_count, 0);
        assert_eq!(stats.failure_count, 0);
        assert_eq!(stats.avg_duration_ms, 0);
        assert!(stats.top_commands.is_empty());
        assert!(stats.top_directories.is_empty());
    }

    #[test]
    fn test_stats_with_entries() {
        let (_temp, repo) = setup_test_db();
        let session = Session::new("host".to_string(), 1000);
        repo.insert_session(&session).unwrap();

        // Insert entries: 3x "git status" (success), 2x "cargo build" (1 success, 1 fail)
        for i in 0..3 {
            let mut entry = Entry::new(
                session.id.clone(),
                "git status".to_string(),
                "/project".to_string(),
                Some(0),
                2000 + i * 100,
                2050 + i * 100,
            );
            entry.executor_type = Some("human".to_string());
            repo.insert_entry(&entry).unwrap();
        }

        let mut entry = Entry::new(
            session.id.clone(),
            "cargo build".to_string(),
            "/project".to_string(),
            Some(0),
            3000,
            4000,
        );
        entry.executor_type = Some("agent".to_string());
        repo.insert_entry(&entry).unwrap();

        let mut entry = Entry::new(
            session.id.clone(),
            "cargo build".to_string(),
            "/other".to_string(),
            Some(1),
            5000,
            5500,
        );
        entry.executor_type = Some("agent".to_string());
        repo.insert_entry(&entry).unwrap();

        let stats = repo.get_stats(None, 10).unwrap();
        assert_eq!(stats.total_commands, 5);
        assert_eq!(stats.unique_commands, 2);
        assert_eq!(stats.success_count, 4);
        assert_eq!(stats.failure_count, 1);

        // Top commands: git status (3) > cargo build (2)
        assert_eq!(stats.top_commands[0].0, "git status");
        assert_eq!(stats.top_commands[0].1, 3);
        assert_eq!(stats.top_commands[1].0, "cargo build");
        assert_eq!(stats.top_commands[1].1, 2);

        // Top directories: /project (4) > /other (1)
        assert_eq!(stats.top_directories[0].0, "/project");
        assert_eq!(stats.top_directories[0].1, 4);

        // Executor: human (3) > agent (2)
        assert_eq!(stats.executor_breakdown[0].0, "human");
        assert_eq!(stats.executor_breakdown[0].1, 3);
        assert_eq!(stats.executor_breakdown[1].0, "agent");
        assert_eq!(stats.executor_breakdown[1].1, 2);
    }

    #[test]
    fn test_stats_with_days_filter() {
        let (_temp, repo) = setup_test_db();
        let session = Session::new("host".to_string(), 1000);
        repo.insert_session(&session).unwrap();

        let now_ms = chrono::Utc::now().timestamp_millis();

        // Recent entry (today)
        let entry = Entry::new(
            session.id.clone(),
            "recent".to_string(),
            "/tmp".to_string(),
            Some(0),
            now_ms - 1000,
            now_ms,
        );
        repo.insert_entry(&entry).unwrap();

        // Old entry (60 days ago)
        let old_ms = now_ms - 60 * 24 * 60 * 60 * 1000;
        let entry = Entry::new(
            session.id.clone(),
            "old".to_string(),
            "/tmp".to_string(),
            Some(0),
            old_ms,
            old_ms + 100,
        );
        repo.insert_entry(&entry).unwrap();

        // All time: 2 commands
        let stats = repo.get_stats(None, 10).unwrap();
        assert_eq!(stats.total_commands, 2);

        // Last 7 days: only 1 command
        let stats = repo.get_stats(Some(7), 10).unwrap();
        assert_eq!(stats.total_commands, 1);
        assert_eq!(stats.top_commands[0].0, "recent");
    }

    // ── Bookmark Tests ──────────────────────────────────────

    #[test]
    fn test_bookmark_crud() {
        let (_dir, repo) = setup_test_db();

        // Empty initially
        let bookmarks = repo.list_bookmarks().unwrap();
        assert!(bookmarks.is_empty());

        // Add bookmarks
        repo.add_bookmark("git status", Some("check repo")).unwrap();
        repo.add_bookmark("cargo test", None).unwrap();

        let bookmarks = repo.list_bookmarks().unwrap();
        assert_eq!(bookmarks.len(), 2);
        assert_eq!(bookmarks[0].command, "cargo test"); // Most recent first
        assert_eq!(bookmarks[1].command, "git status");
        assert_eq!(bookmarks[1].label.as_deref(), Some("check repo"));

        // Remove one
        let removed = repo.remove_bookmark("git status").unwrap();
        assert!(removed);

        let bookmarks = repo.list_bookmarks().unwrap();
        assert_eq!(bookmarks.len(), 1);
        assert_eq!(bookmarks[0].command, "cargo test");

        // Remove non-existent
        let removed = repo.remove_bookmark("nonexistent").unwrap();
        assert!(!removed);
    }

    #[test]
    fn test_bookmark_duplicate_upsert() {
        let (_dir, repo) = setup_test_db();

        repo.add_bookmark("git push", Some("deploy")).unwrap();
        // Re-adding same command replaces (INSERT OR REPLACE)
        repo.add_bookmark("git push", Some("updated label"))
            .unwrap();

        let bookmarks = repo.list_bookmarks().unwrap();
        assert_eq!(bookmarks.len(), 1);
        assert_eq!(bookmarks[0].label.as_deref(), Some("updated label"));
    }

    #[test]
    fn test_get_bookmarked_commands() {
        let (_dir, repo) = setup_test_db();

        repo.add_bookmark("ls -la", None).unwrap();
        repo.add_bookmark("pwd", None).unwrap();

        let set = repo.get_bookmarked_commands().unwrap();
        assert_eq!(set.len(), 2);
        assert!(set.contains("ls -la"));
        assert!(set.contains("pwd"));
    }

    // ── Directory-Scoped Tests ──────────────────────────────

    #[test]
    fn test_filter_by_cwd() {
        let (_dir, repo) = setup_test_db();
        let session = Session::new("host".to_string(), 100);
        repo.insert_session(&session).unwrap();

        // Insert entries in different directories
        repo.insert_entry(&Entry::new(
            session.id.clone(),
            "cargo build".into(),
            "/home/user/project".into(),
            Some(0),
            1000,
            1100,
        ))
        .unwrap();
        repo.insert_entry(&Entry::new(
            session.id.clone(),
            "npm test".into(),
            "/home/user/webapp".into(),
            Some(0),
            2000,
            2100,
        ))
        .unwrap();
        repo.insert_entry(&Entry::new(
            session.id.clone(),
            "cargo test".into(),
            "/home/user/project".into(),
            Some(0),
            3000,
            3100,
        ))
        .unwrap();

        // Filter by cwd
        let project_entries = repo
            .get_entries(
                10,
                0,
                None,
                None,
                None,
                None,
                None,
                false,
                None,
                Some("/home/user/project"),
            )
            .unwrap();
        assert_eq!(project_entries.len(), 2);
        assert!(project_entries
            .iter()
            .all(|e| e.cwd == "/home/user/project"));

        let webapp_entries = repo
            .get_entries(
                10,
                0,
                None,
                None,
                None,
                None,
                None,
                false,
                None,
                Some("/home/user/webapp"),
            )
            .unwrap();
        assert_eq!(webapp_entries.len(), 1);
        assert_eq!(webapp_entries[0].command, "npm test");

        // No filter returns all
        let all_entries = repo
            .get_entries(10, 0, None, None, None, None, None, false, None, None)
            .unwrap();
        assert_eq!(all_entries.len(), 3);

        // Count with cwd filter
        let project_count = repo
            .count_filtered_entries(
                None,
                None,
                None,
                None,
                None,
                false,
                None,
                Some("/home/user/project"),
            )
            .unwrap();
        assert_eq!(project_count, 2);
    }

    #[test]
    fn test_cwd_filter_with_other_filters() {
        let (_dir, repo) = setup_test_db();
        let session = Session::new("host".to_string(), 100);
        repo.insert_session(&session).unwrap();

        let mut entry1 = Entry::new(
            session.id.clone(),
            "cargo build".into(),
            "/home/user/project".into(),
            Some(0),
            1000,
            1100,
        );
        entry1.executor_type = Some("human".to_string());
        repo.insert_entry(&entry1).unwrap();

        let mut entry2 = Entry::new(
            session.id.clone(),
            "cargo test".into(),
            "/home/user/project".into(),
            Some(1),
            2000,
            2100,
        );
        entry2.executor_type = Some("agent".to_string());
        repo.insert_entry(&entry2).unwrap();

        // cwd + executor filter
        let human_project = repo
            .get_entries(
                10,
                0,
                None,
                None,
                None,
                None,
                None,
                false,
                Some("human"),
                Some("/home/user/project"),
            )
            .unwrap();
        assert_eq!(human_project.len(), 1);
        assert_eq!(human_project[0].command, "cargo build");

        // cwd + exit code filter
        let failed_project = repo
            .get_entries(
                10,
                0,
                None,
                None,
                None,
                Some(1),
                None,
                false,
                None,
                Some("/home/user/project"),
            )
            .unwrap();
        assert_eq!(failed_project.len(), 1);
        assert_eq!(failed_project[0].command, "cargo test");
    }

    #[test]
    fn test_note_crud() {
        let (_dir, repo) = setup_test_db();
        let session = Session::new("host".to_string(), 100);
        repo.insert_session(&session).unwrap();

        let entry_id = repo
            .insert_entry(&Entry::new(
                session.id.clone(),
                "cargo build".into(),
                "/tmp".into(),
                Some(0),
                1000,
                1100,
            ))
            .unwrap();

        // No note initially
        assert!(repo.get_note(entry_id).unwrap().is_none());

        // Create note
        repo.upsert_note(entry_id, "Fixed the SSL bug").unwrap();
        let note = repo.get_note(entry_id).unwrap().unwrap();
        assert_eq!(note.entry_id, entry_id);
        assert_eq!(note.content, "Fixed the SSL bug");

        // Delete note
        assert!(repo.delete_note(entry_id).unwrap());
        assert!(repo.get_note(entry_id).unwrap().is_none());

        // Delete non-existent returns false
        assert!(!repo.delete_note(entry_id).unwrap());
    }

    #[test]
    fn test_note_upsert_overwrites() {
        let (_dir, repo) = setup_test_db();
        let session = Session::new("host".to_string(), 100);
        repo.insert_session(&session).unwrap();

        let entry_id = repo
            .insert_entry(&Entry::new(
                session.id.clone(),
                "git push".into(),
                "/tmp".into(),
                Some(0),
                1000,
                1100,
            ))
            .unwrap();

        repo.upsert_note(entry_id, "First note").unwrap();
        let note1 = repo.get_note(entry_id).unwrap().unwrap();
        assert_eq!(note1.content, "First note");

        repo.upsert_note(entry_id, "Updated note").unwrap();
        let note2 = repo.get_note(entry_id).unwrap().unwrap();
        assert_eq!(note2.content, "Updated note");
        assert_eq!(note2.id, note1.id); // Same row, updated in place
    }

    #[test]
    fn test_get_noted_entry_ids() {
        let (_dir, repo) = setup_test_db();
        let session = Session::new("host".to_string(), 100);
        repo.insert_session(&session).unwrap();

        let id1 = repo
            .insert_entry(&Entry::new(
                session.id.clone(),
                "cmd1".into(),
                "/tmp".into(),
                Some(0),
                1000,
                1100,
            ))
            .unwrap();
        let id2 = repo
            .insert_entry(&Entry::new(
                session.id.clone(),
                "cmd2".into(),
                "/tmp".into(),
                Some(0),
                2000,
                2100,
            ))
            .unwrap();
        let id3 = repo
            .insert_entry(&Entry::new(
                session.id.clone(),
                "cmd3".into(),
                "/tmp".into(),
                Some(0),
                3000,
                3100,
            ))
            .unwrap();

        // Empty initially
        let ids = repo.get_noted_entry_ids().unwrap();
        assert!(ids.is_empty());

        // Add notes to entries 1 and 3
        repo.upsert_note(id1, "note for cmd1").unwrap();
        repo.upsert_note(id3, "note for cmd3").unwrap();

        let ids = repo.get_noted_entry_ids().unwrap();
        assert_eq!(ids.len(), 2);
        assert!(ids.contains(&id1));
        assert!(!ids.contains(&id2));
        assert!(ids.contains(&id3));
    }

    #[test]
    fn test_get_frequent_commands() {
        let (_dir, repo) = setup_test_db();
        let session = Session::new("host".to_string(), 100);
        repo.insert_session(&session).unwrap();

        let now = chrono::Utc::now().timestamp();

        // Insert "cargo build --release" 10 times (long, frequent)
        for i in 0..10 {
            repo.insert_entry(&Entry::new(
                session.id.clone(),
                "cargo build --release".into(),
                "/project".into(),
                Some(0),
                now + i,
                now + i + 50,
            ))
            .unwrap();
        }

        // Insert "ls" 20 times (short, frequent)
        for i in 0..20 {
            repo.insert_entry(&Entry::new(
                session.id.clone(),
                "ls".into(),
                "/tmp".into(),
                Some(0),
                now + 100 + i,
                now + 100 + i + 10,
            ))
            .unwrap();
        }

        // Insert "git status" 3 times (long enough, but below min_count)
        for i in 0..3 {
            repo.insert_entry(&Entry::new(
                session.id.clone(),
                "git status --short".into(),
                "/project".into(),
                Some(0),
                now + 200 + i,
                now + 200 + i + 10,
            ))
            .unwrap();
        }

        // min_length=12 should exclude "ls", min_count=5 should exclude "git status --short"
        let results = repo.get_frequent_commands(None, 5, 12, 10).unwrap();

        assert_eq!(results.len(), 1);
        assert_eq!(results[0].0, "cargo build --release");
        assert_eq!(results[0].1, 10);
        assert_eq!(results[0].2, 1); // all from /project
    }

    #[test]
    fn test_get_frequent_commands_with_days() {
        let (_dir, repo) = setup_test_db();
        let session = Session::new("host".to_string(), 100);
        repo.insert_session(&session).unwrap();

        let now = chrono::Utc::now().timestamp();
        let old = now - 100 * 86400; // 100 days ago

        // Old commands
        for i in 0..10 {
            repo.insert_entry(&Entry::new(
                session.id.clone(),
                "cargo build --release".into(),
                "/project".into(),
                Some(0),
                old + i,
                old + i + 50,
            ))
            .unwrap();
        }

        // Recent commands
        for i in 0..10 {
            repo.insert_entry(&Entry::new(
                session.id.clone(),
                "cargo test --workspace".into(),
                "/project".into(),
                Some(0),
                now + i,
                now + i + 50,
            ))
            .unwrap();
        }

        // With days=30, only recent commands
        let results = repo.get_frequent_commands(Some(30), 5, 12, 10).unwrap();

        assert_eq!(results.len(), 1);
        assert_eq!(results[0].0, "cargo test --workspace");
        assert_eq!(results[0].2, 1); // all from /project
    }

    #[test]
    fn test_get_frequent_commands_dir_diversity_ranking() {
        let (_dir, repo) = setup_test_db();
        let session = Session::new("host".to_string(), 100);
        repo.insert_session(&session).unwrap();

        let now = chrono::Utc::now().timestamp();

        // Command A: 10 uses from 1 directory → score = 10 * 1 = 10
        for i in 0..10 {
            repo.insert_entry(&Entry::new(
                session.id.clone(),
                "cargo build --release".into(),
                "/project-a".into(),
                Some(0),
                now + i,
                now + i + 50,
            ))
            .unwrap();
        }

        // Command B: 8 uses from 4 directories → score = 8 * 4 = 32
        let dirs = ["/proj1", "/proj2", "/proj3", "/proj4"];
        for i in 0..8 {
            repo.insert_entry(&Entry::new(
                session.id.clone(),
                "git log --oneline".into(),
                dirs[i % 4].into(),
                Some(0),
                now + 100 + i as i64,
                now + 100 + i as i64 + 50,
            ))
            .unwrap();
        }

        let results = repo.get_frequent_commands(None, 5, 12, 10).unwrap();

        // "git log --oneline" should rank first despite fewer uses (higher dir diversity)
        assert_eq!(results.len(), 2);
        assert_eq!(results[0].0, "git log --oneline");
        assert_eq!(results[0].1, 8);
        assert_eq!(results[0].2, 4);
        assert_eq!(results[1].0, "cargo build --release");
        assert_eq!(results[1].1, 10);
        assert_eq!(results[1].2, 1);
    }
}
