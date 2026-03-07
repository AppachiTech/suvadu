mod aliases;
mod bookmarks;
mod entries;
mod notes;
mod stats;
mod tags;

#[cfg(test)]
mod tests;

use crate::db::DbResult;
use crate::models::{Entry, Session};
use rusqlite::{params, Connection};

/// Shared entry column list for SELECT queries
pub const ENTRY_COLUMNS: &str = "e.id, e.session_id, e.command, e.cwd, e.exit_code, e.started_at, e.ended_at, e.duration_ms, e.context, COALESCE(et.name, st.name) as tag_name, e.tag_id, e.executor_type, e.executor";

/// Shared FROM/JOIN clause for entry queries
pub const ENTRY_JOINS: &str = "FROM entries e
             JOIN sessions s ON e.session_id = s.id
             LEFT JOIN tags st ON s.tag_id = st.id
             LEFT JOIN tags et ON e.tag_id = et.id";

/// Maps a `SQLite` row to an `Entry`. `tag_id_col` is the column index where `tag_id` starts
/// (10 for standard queries, 11 for unique queries where COUNT(*) is at position 10).
pub fn entry_from_row(row: &rusqlite::Row, tag_id_col: usize) -> rusqlite::Result<Entry> {
    let context_str: Option<String> = row.get(8)?;
    let context = context_str.and_then(|s| match serde_json::from_str(&s) {
        Ok(ctx) => Some(ctx),
        Err(e) => {
            eprintln!("suvadu: malformed context JSON: {e}");
            None
        }
    });
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
pub struct FilterBuilder {
    clauses: Vec<String>,
    params: Vec<Box<dyn rusqlite::ToSql>>,
}

impl FilterBuilder {
    pub fn new() -> Self {
        Self {
            clauses: Vec::new(),
            params: Vec::new(),
        }
    }

    pub fn with_date_range(mut self, after: Option<i64>, before: Option<i64>) -> Self {
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

    pub fn with_tag(mut self, tag_id: Option<i64>) -> Self {
        if let Some(tid) = tag_id {
            self.clauses.push("(s.tag_id = ? OR e.tag_id = ?)".into());
            self.params.push(Box::new(tid));
            self.params.push(Box::new(tid));
        }
        self
    }

    pub fn with_exit_code(mut self, exit_code: Option<i32>) -> Self {
        if let Some(code) = exit_code {
            self.clauses.push("e.exit_code = ?".into());
            self.params.push(Box::new(code));
        }
        self
    }

    pub fn with_query(self, query: Option<&str>, prefix_match: bool) -> Self {
        self.with_query_field(query, prefix_match, "command")
    }

    pub fn with_query_field(
        mut self,
        query: Option<&str>,
        prefix_match: bool,
        field: &str,
    ) -> Self {
        if let Some(q) = query {
            let column = match field {
                "cwd" => "e.cwd",
                "session" => "e.session_id",
                "executor" => "COALESCE(e.executor_type || ' ' || e.executor, '')",
                _ => "e.command",
            };
            self.clauses.push(format!("{column} LIKE ?"));
            if prefix_match {
                self.params.push(Box::new(format!("{q}%")));
            } else {
                self.params.push(Box::new(format!("%{q}%")));
            }
        }
        self
    }

    pub fn with_cwd(mut self, cwd: Option<&str>) -> Self {
        if let Some(dir) = cwd {
            self.clauses.push("e.cwd = ?".into());
            self.params.push(Box::new(dir.to_string()));
        }
        self
    }

    pub fn with_session(mut self, session_id: Option<&str>) -> Self {
        if let Some(sid) = session_id {
            self.clauses.push("e.session_id = ?".into());
            self.params.push(Box::new(sid.to_string()));
        }
        self
    }

    pub fn with_executor(mut self, executor: Option<&str>) -> Self {
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

    pub fn build_where(&self) -> String {
        if self.clauses.is_empty() {
            " WHERE 1=1".into()
        } else {
            format!(" WHERE {}", self.clauses.join(" AND "))
        }
    }

    pub fn params_refs(&self) -> Vec<&dyn rusqlite::ToSql> {
        self.params
            .iter()
            .map(std::convert::AsRef::as_ref)
            .collect()
    }

    pub fn push_param(&mut self, val: Box<dyn rusqlite::ToSql>) {
        self.params.push(val);
    }
}

/// Repository for managing history entries and sessions
pub struct Repository {
    conn: Connection,
}

impl Repository {
    /// Create a new repository with the given connection
    pub const fn new(conn: Connection) -> Self {
        Self { conn }
    }

    /// Open the database and return a ready-to-use repository.
    pub fn init() -> crate::db::DbResult<Self> {
        let db_path = crate::db::get_db_path()?;
        let conn = crate::db::init_db(&db_path)?;
        Ok(Self::new(conn))
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

    /// Roll back a transaction (undo all changes since `begin_transaction`)
    pub fn rollback(&self) -> DbResult<()> {
        self.conn.execute_batch("ROLLBACK")?;
        Ok(())
    }

    /// Get all (command, `started_at_ms`) pairs for dedup during import.
    /// Uses millisecond precision to avoid collisions between distinct entries
    /// that happen to share the same second. Callers importing second-precision
    /// sources (e.g. zsh-history) should round their timestamps before lookup.
    pub fn get_existing_command_timestamps(
        &self,
    ) -> DbResult<std::collections::HashSet<(String, i64)>> {
        let mut stmt = self
            .conn
            .prepare("SELECT command, started_at FROM entries")?;
        let rows = stmt.query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
        })?;
        let mut set = std::collections::HashSet::new();
        for row in rows {
            set.insert(row?);
        }
        Ok(set)
    }
}
