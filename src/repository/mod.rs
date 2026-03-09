mod aliases;
mod api;
mod bookmarks;
mod entries;
mod notes;
mod stats;
mod tags;

// Re-exported for use as a mockable trait boundary in tests and downstream consumers.
#[allow(unused_imports)]
pub use api::RepositoryApi;

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

/// Filter parameters for entry queries. Replaces 10-12 positional parameters
/// with a single self-documenting struct.
#[derive(Default, Clone)]
pub struct QueryFilter<'a> {
    pub after: Option<i64>,
    pub before: Option<i64>,
    pub tag_id: Option<i64>,
    pub exit_code: Option<i32>,
    pub query: Option<&'a str>,
    pub prefix_match: bool,
    pub executor: Option<&'a str>,
    pub cwd: Option<&'a str>,
    pub field: crate::models::SearchField,
}

impl QueryFilter<'_> {
    /// Build a `FilterBuilder` from this filter.
    pub fn to_filter_builder(&self) -> FilterBuilder {
        FilterBuilder::new()
            .with_date_range(self.after, self.before)
            .with_tag(self.tag_id)
            .with_exit_code(self.exit_code)
            .with_query_field(self.query, self.prefix_match, self.field)
            .with_executor(self.executor)
            .with_cwd(self.cwd)
    }
}

use crate::models::SearchField;

/// Filter parameters for replay queries.
#[derive(Default)]
pub struct ReplayFilter<'a> {
    pub after: Option<i64>,
    pub before: Option<i64>,
    pub tag_id: Option<i64>,
    pub exit_code: Option<i32>,
    pub executor: Option<&'a str>,
    pub cwd: Option<&'a str>,
    /// Maximum number of entries to return (None = unlimited).
    pub limit: Option<usize>,
}

/// Escape SQL LIKE wildcards (`%`, `_`) and the escape character (`\`) in user input.
fn escape_like(s: &str) -> String {
    s.replace('\\', "\\\\")
        .replace('%', "\\%")
        .replace('_', "\\_")
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
        self.with_query_field(query, prefix_match, SearchField::Command)
    }

    pub fn with_query_field(
        mut self,
        query: Option<&str>,
        prefix_match: bool,
        field: SearchField,
    ) -> Self {
        if let Some(q) = query {
            let column = match field {
                SearchField::Cwd => "e.cwd",
                SearchField::Session => "e.session_id",
                SearchField::Executor => "COALESCE(e.executor_type || ' ' || e.executor, '')",
                SearchField::Command => "e.command",
            };
            self.clauses.push(format!("{column} LIKE ? ESCAPE '\\'"));
            let escaped = escape_like(q);
            if prefix_match {
                self.params.push(Box::new(format!("{escaped}%")));
            } else {
                self.params.push(Box::new(format!("%{escaped}%")));
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

    /// Filter on `s.created_at >= ?` — for session-centric queries
    /// where the primary table is `sessions s`.
    pub fn with_session_created_after(mut self, after: Option<i64>) -> Self {
        if let Some(ts) = after {
            self.clauses.push("s.created_at >= ?".into());
            self.params.push(Box::new(ts));
        }
        self
    }

    /// Filter on `s.tag_id = ?` — for session-centric queries.
    pub fn with_session_tag(mut self, tag_id: Option<i64>) -> Self {
        if let Some(tid) = tag_id {
            self.clauses.push("s.tag_id = ?".into());
            self.params.push(Box::new(tid));
        }
        self
    }

    pub fn with_executor(mut self, executor: Option<&str>) -> Self {
        if let Some(exec) = executor {
            self.clauses.push(
                "(e.executor_type LIKE ? ESCAPE '\\' OR e.executor LIKE ? ESCAPE '\\' OR (e.executor_type || '-' || e.executor) LIKE ? ESCAPE '\\')".into(),
            );
            let escaped = escape_like(exec);
            let pattern = format!("%{escaped}%");
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

#[cfg(test)]
mod filter_builder_tests {
    use super::FilterBuilder;
    use crate::models::SearchField;

    #[test]
    fn empty_builder_produces_where_1_eq_1() {
        let fb = FilterBuilder::new();
        assert_eq!(fb.build_where(), " WHERE 1=1");
        assert!(fb.params_refs().is_empty());
    }

    #[test]
    fn with_date_range_after_only() {
        let fb = FilterBuilder::new().with_date_range(Some(1000), None);
        assert_eq!(fb.build_where(), " WHERE e.started_at >= ?");
        assert_eq!(fb.params_refs().len(), 1);
    }

    #[test]
    fn with_date_range_before_only() {
        let fb = FilterBuilder::new().with_date_range(None, Some(2000));
        assert_eq!(fb.build_where(), " WHERE e.started_at <= ?");
        assert_eq!(fb.params_refs().len(), 1);
    }

    #[test]
    fn with_date_range_both() {
        let fb = FilterBuilder::new().with_date_range(Some(1000), Some(2000));
        assert_eq!(
            fb.build_where(),
            " WHERE e.started_at >= ? AND e.started_at <= ?"
        );
        assert_eq!(fb.params_refs().len(), 2);
    }

    #[test]
    fn with_tag_adds_two_params() {
        let fb = FilterBuilder::new().with_tag(Some(42));
        assert_eq!(fb.build_where(), " WHERE (s.tag_id = ? OR e.tag_id = ?)");
        assert_eq!(fb.params_refs().len(), 2);
    }

    #[test]
    fn with_tag_none_is_noop() {
        let fb = FilterBuilder::new().with_tag(None);
        assert_eq!(fb.build_where(), " WHERE 1=1");
        assert!(fb.params_refs().is_empty());
    }

    #[test]
    fn with_exit_code() {
        let fb = FilterBuilder::new().with_exit_code(Some(0));
        assert_eq!(fb.build_where(), " WHERE e.exit_code = ?");
        assert_eq!(fb.params_refs().len(), 1);
    }

    #[test]
    fn with_query_contains_mode() {
        let fb = FilterBuilder::new().with_query(Some("git"), false);
        assert_eq!(fb.build_where(), " WHERE e.command LIKE ? ESCAPE '\\'");
        assert_eq!(fb.params_refs().len(), 1);
    }

    #[test]
    fn with_query_prefix_mode() {
        let fb = FilterBuilder::new().with_query(Some("git"), true);
        assert_eq!(fb.build_where(), " WHERE e.command LIKE ? ESCAPE '\\'");
        assert_eq!(fb.params_refs().len(), 1);
    }

    #[test]
    fn with_query_field_cwd() {
        let fb = FilterBuilder::new().with_query_field(Some("home"), false, SearchField::Cwd);
        assert_eq!(fb.build_where(), " WHERE e.cwd LIKE ? ESCAPE '\\'");
    }

    #[test]
    fn with_query_field_session() {
        let fb = FilterBuilder::new().with_query_field(Some("abc"), false, SearchField::Session);
        assert_eq!(fb.build_where(), " WHERE e.session_id LIKE ? ESCAPE '\\'");
    }

    #[test]
    fn with_query_field_executor() {
        let fb =
            FilterBuilder::new().with_query_field(Some("claude"), false, SearchField::Executor);
        assert!(fb.build_where().contains("COALESCE"));
        assert!(fb.build_where().contains("ESCAPE '\\'"));
    }

    #[test]
    fn with_cwd() {
        let fb = FilterBuilder::new().with_cwd(Some("/home/user"));
        assert_eq!(fb.build_where(), " WHERE e.cwd = ?");
        assert_eq!(fb.params_refs().len(), 1);
    }

    #[test]
    fn with_session() {
        let fb = FilterBuilder::new().with_session(Some("sess-123"));
        assert_eq!(fb.build_where(), " WHERE e.session_id = ?");
        assert_eq!(fb.params_refs().len(), 1);
    }

    #[test]
    fn with_executor() {
        let fb = FilterBuilder::new().with_executor(Some("claude"));
        assert!(fb.build_where().contains("e.executor_type LIKE ?"));
        assert_eq!(fb.params_refs().len(), 3);
    }

    #[test]
    fn chained_filters() {
        let fb = FilterBuilder::new()
            .with_date_range(Some(1000), None)
            .with_tag(Some(5))
            .with_exit_code(Some(0))
            .with_query(Some("cargo"), false);
        let where_clause = fb.build_where();
        assert!(where_clause.contains("e.started_at >= ?"));
        assert!(where_clause.contains("s.tag_id = ? OR e.tag_id = ?"));
        assert!(where_clause.contains("e.exit_code = ?"));
        assert!(where_clause.contains("e.command LIKE ?"));
        // 1 (date) + 2 (tag) + 1 (exit) + 1 (query) = 5
        assert_eq!(fb.params_refs().len(), 5);
    }

    #[test]
    fn push_param_adds_to_list() {
        let mut fb = FilterBuilder::new();
        fb.push_param(Box::new(42_i64));
        assert_eq!(fb.params_refs().len(), 1);
    }

    #[test]
    fn all_none_filters_produce_no_clauses() {
        let fb = FilterBuilder::new()
            .with_date_range(None, None)
            .with_tag(None)
            .with_exit_code(None)
            .with_query(None, false)
            .with_cwd(None)
            .with_session(None)
            .with_executor(None);
        assert_eq!(fb.build_where(), " WHERE 1=1");
        assert!(fb.params_refs().is_empty());
    }

    #[test]
    fn with_session_created_after() {
        let fb = FilterBuilder::new().with_session_created_after(Some(5000));
        assert_eq!(fb.build_where(), " WHERE s.created_at >= ?");
        assert_eq!(fb.params_refs().len(), 1);
    }

    #[test]
    fn with_session_created_after_none_is_noop() {
        let fb = FilterBuilder::new().with_session_created_after(None);
        assert_eq!(fb.build_where(), " WHERE 1=1");
        assert!(fb.params_refs().is_empty());
    }

    #[test]
    fn with_session_tag() {
        let fb = FilterBuilder::new().with_session_tag(Some(7));
        assert_eq!(fb.build_where(), " WHERE s.tag_id = ?");
        assert_eq!(fb.params_refs().len(), 1);
    }

    #[test]
    fn with_session_tag_none_is_noop() {
        let fb = FilterBuilder::new().with_session_tag(None);
        assert_eq!(fb.build_where(), " WHERE 1=1");
        assert!(fb.params_refs().is_empty());
    }

    #[test]
    fn chained_session_filters() {
        let fb = FilterBuilder::new()
            .with_session_created_after(Some(1000))
            .with_session_tag(Some(3));
        let where_clause = fb.build_where();
        assert!(where_clause.contains("s.created_at >= ?"));
        assert!(where_clause.contains("s.tag_id = ?"));
        assert_eq!(fb.params_refs().len(), 2);
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
            "INSERT INTO sessions (id, hostname, created_at, tag_id) VALUES (?1, ?2, ?3, ?4)",
            params![
                session.id,
                session.hostname,
                session.created_at,
                session.tag_id
            ],
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

    /// Check if a (command, `started_at`) pair already exists in the database.
    /// Used during import dedup — avoids loading the entire history into memory.
    pub fn entry_exists(&self, command: &str, started_at: i64) -> DbResult<bool> {
        let mut stmt = self
            .conn
            .prepare_cached("SELECT 1 FROM entries WHERE command = ? AND started_at = ? LIMIT 1")?;
        let exists = stmt.exists(params![command, started_at])?;
        Ok(exists)
    }
}
