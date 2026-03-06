use rusqlite::{params, Connection};
use std::path::PathBuf;
use thiserror::Error;

#[derive(Error, Debug)]
pub enum DbError {
    #[error("SQLite error: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
    #[error("Database path error: {0}")]
    Path(String),
    #[error("Validation error: {0}")]
    Validation(String),
}

pub type DbResult<T> = Result<T, DbError>;

/// Current schema version. Increment when adding new migrations.
const SCHEMA_VERSION: i64 = 1;

/// Get the path to the suvadu database file
pub fn get_db_path() -> DbResult<PathBuf> {
    let data_dir = directories::ProjectDirs::from("tech", "appachi", "suvadu")
        .ok_or_else(|| DbError::Path("Could not determine data directory".to_string()))?
        .data_dir()
        .to_path_buf();

    std::fs::create_dir_all(&data_dir)?;
    Ok(data_dir.join("history.db"))
}

/// Read the current schema version (0 if no version table exists yet).
fn get_schema_version(conn: &Connection) -> DbResult<i64> {
    conn.execute(
        "CREATE TABLE IF NOT EXISTS schema_version (version INTEGER NOT NULL)",
        [],
    )?;

    let version: i64 = conn
        .query_row("SELECT version FROM schema_version LIMIT 1", [], |row| {
            row.get(0)
        })
        .unwrap_or(0);

    Ok(version)
}

/// Set the schema version after a successful migration.
fn set_schema_version(conn: &Connection, version: i64) -> DbResult<()> {
    conn.execute("DELETE FROM schema_version", [])?;
    conn.execute(
        "INSERT INTO schema_version (version) VALUES (?1)",
        params![version],
    )?;
    Ok(())
}

/// Check whether a column exists on a table.
fn column_exists(conn: &Connection, table: &str, column: &str) -> bool {
    conn.query_row(
        &format!("SELECT COUNT(*) FROM pragma_table_info('{table}') WHERE name='{column}'"),
        [],
        |row| row.get::<_, i64>(0),
    )
    .map(|count| count > 0)
    .unwrap_or(false)
}

/// Migration v1: full schema as of initial release.
///
/// Every statement is idempotent (`IF NOT EXISTS` / column-existence
/// guards) so it is safe to run against both fresh and pre-existing
/// databases that were created before schema versioning was added.
fn migrate_v1(conn: &Connection) -> DbResult<()> {
    // ── Tags ────────────────────────────────────────────
    conn.execute(
        "CREATE TABLE IF NOT EXISTS tags (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            name TEXT UNIQUE NOT NULL,
            description TEXT
        )",
        [],
    )?;

    // ── Sessions ────────────────────────────────────────
    conn.execute(
        "CREATE TABLE IF NOT EXISTS sessions (
            id TEXT PRIMARY KEY,
            hostname TEXT NOT NULL,
            created_at INTEGER NOT NULL,
            tag_id INTEGER REFERENCES tags(id)
        )",
        [],
    )?;

    if !column_exists(conn, "sessions", "tag_id") {
        conn.execute(
            "ALTER TABLE sessions ADD COLUMN tag_id INTEGER REFERENCES tags(id)",
            [],
        )?;
    }

    // ── Entries ─────────────────────────────────────────
    conn.execute(
        "CREATE TABLE IF NOT EXISTS entries (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            session_id TEXT NOT NULL,
            command TEXT NOT NULL,
            cwd TEXT NOT NULL,
            exit_code INTEGER,
            started_at INTEGER NOT NULL,
            ended_at INTEGER NOT NULL,
            duration_ms INTEGER NOT NULL,
            context TEXT,
            FOREIGN KEY (session_id) REFERENCES sessions(id)
        )",
        [],
    )?;

    if !column_exists(conn, "entries", "tag_id") {
        conn.execute(
            "ALTER TABLE entries ADD COLUMN tag_id INTEGER REFERENCES tags(id)",
            [],
        )?;
    }

    if !column_exists(conn, "entries", "executor_type") {
        conn.execute("ALTER TABLE entries ADD COLUMN executor_type TEXT", [])?;
    }

    if !column_exists(conn, "entries", "executor") {
        conn.execute("ALTER TABLE entries ADD COLUMN executor TEXT", [])?;
    }

    // ── Indexes ─────────────────────────────────────────
    conn.execute_batch(
        "CREATE INDEX IF NOT EXISTS idx_entries_session_id ON entries(session_id);
         CREATE INDEX IF NOT EXISTS idx_entries_started_at ON entries(started_at);
         CREATE INDEX IF NOT EXISTS idx_entries_command    ON entries(command);
         CREATE INDEX IF NOT EXISTS idx_entries_tag_id     ON entries(tag_id);",
    )?;

    // ── Bookmarks ───────────────────────────────────────
    conn.execute(
        "CREATE TABLE IF NOT EXISTS bookmarks (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            command TEXT NOT NULL UNIQUE,
            label TEXT,
            created_at INTEGER NOT NULL
        )",
        [],
    )?;
    conn.execute(
        "CREATE INDEX IF NOT EXISTS idx_bookmarks_command ON bookmarks(command)",
        [],
    )?;

    // ── Notes ───────────────────────────────────────────
    conn.execute(
        "CREATE TABLE IF NOT EXISTS notes (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            entry_id INTEGER NOT NULL UNIQUE REFERENCES entries(id) ON DELETE CASCADE,
            note TEXT NOT NULL,
            created_at INTEGER NOT NULL,
            updated_at INTEGER NOT NULL
        )",
        [],
    )?;
    conn.execute(
        "CREATE INDEX IF NOT EXISTS idx_notes_entry_id ON notes(entry_id)",
        [],
    )?;

    Ok(())
}

/// Initialize the database with proper schema and settings.
///
/// Migrations are tracked via a `schema_version` table so each
/// migration runs exactly once. All migration functions are
/// idempotent as an extra safety net.
pub fn init_db(path: &PathBuf) -> DbResult<Connection> {
    let conn = Connection::open(path)?;

    // Enable WAL mode for better concurrency and performance
    conn.pragma_update(None, "journal_mode", "WAL")?;
    conn.pragma_update(None, "synchronous", "NORMAL")?;

    let version = get_schema_version(&conn)?;

    if version < 1 {
        migrate_v1(&conn)?;
        set_schema_version(&conn, SCHEMA_VERSION)?;
    }

    // Future migrations:
    // if version < 2 {
    //     migrate_v2(&conn)?;
    //     set_schema_version(&conn, 2)?;
    // }

    Ok(conn)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn test_init_db() {
        let temp_dir = TempDir::new().unwrap();
        let db_path = temp_dir.path().join("test.db");

        let conn = init_db(&db_path).unwrap();

        // Verify WAL mode is enabled
        let journal_mode: String = conn
            .pragma_query_value(None, "journal_mode", |row| row.get(0))
            .unwrap();
        assert_eq!(journal_mode.to_lowercase(), "wal");

        // Verify tables were created
        let table_count: i32 = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name IN ('sessions', 'entries')",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(table_count, 2);
    }

    #[test]
    fn test_get_db_path_returns_valid_path() {
        let path = get_db_path().expect("get_db_path should succeed");
        let path_str = path.to_string_lossy().to_string();

        // Should end with history.db
        assert!(
            path_str.ends_with("history.db"),
            "DB path should end with history.db, got: {path_str}"
        );

        // Should contain suvadu in the path (platform-agnostic check)
        let path_lower = path_str.to_lowercase();
        assert!(
            path_lower.contains("suvadu"),
            "DB path should contain 'suvadu', got: {path_str}"
        );

        // Should be an absolute path
        assert!(
            path.is_absolute(),
            "DB path should be absolute, got: {path_str}"
        );
    }

    #[test]
    fn test_schema_version_tracking() {
        let temp_dir = TempDir::new().unwrap();
        let db_path = temp_dir.path().join("test.db");

        let conn = init_db(&db_path).unwrap();

        // After init, version should be SCHEMA_VERSION
        let version = get_schema_version(&conn).unwrap();
        assert_eq!(version, SCHEMA_VERSION);
    }

    #[test]
    fn test_schema_version_table_exists() {
        let temp_dir = TempDir::new().unwrap();
        let db_path = temp_dir.path().join("test.db");

        let conn = init_db(&db_path).unwrap();

        let table_exists: bool = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='schema_version'",
                [],
                |row| row.get::<_, i64>(0),
            )
            .map(|c| c > 0)
            .unwrap();
        assert!(table_exists);
    }

    #[test]
    fn test_init_db_idempotent() {
        let temp_dir = TempDir::new().unwrap();
        let db_path = temp_dir.path().join("test.db");

        // First init
        let conn = init_db(&db_path).unwrap();
        let v1 = get_schema_version(&conn).unwrap();
        drop(conn);

        // Second init — should not fail or change version
        let conn = init_db(&db_path).unwrap();
        let v2 = get_schema_version(&conn).unwrap();

        assert_eq!(v1, v2);
        assert_eq!(v2, SCHEMA_VERSION);
    }

    #[test]
    fn test_migrate_pre_existing_db_without_version() {
        let temp_dir = TempDir::new().unwrap();
        let db_path = temp_dir.path().join("test.db");

        // Simulate a pre-existing database without schema_version table
        let conn = Connection::open(&db_path).unwrap();
        conn.execute_batch(
            "CREATE TABLE tags (id INTEGER PRIMARY KEY AUTOINCREMENT, name TEXT UNIQUE NOT NULL, description TEXT);
             CREATE TABLE sessions (id TEXT PRIMARY KEY, hostname TEXT NOT NULL, created_at INTEGER NOT NULL, tag_id INTEGER REFERENCES tags(id));
             CREATE TABLE entries (id INTEGER PRIMARY KEY AUTOINCREMENT, session_id TEXT NOT NULL, command TEXT NOT NULL, cwd TEXT NOT NULL, exit_code INTEGER, started_at INTEGER NOT NULL, ended_at INTEGER NOT NULL, duration_ms INTEGER NOT NULL, context TEXT, tag_id INTEGER, executor_type TEXT, executor TEXT, FOREIGN KEY (session_id) REFERENCES sessions(id));
             INSERT INTO sessions VALUES ('s1', 'host', 1000, NULL);
             INSERT INTO entries VALUES (1, 's1', 'ls', '/tmp', 0, 1000, 1100, 100, NULL, NULL, NULL, NULL);",
        ).unwrap();
        drop(conn);

        // Now init_db should detect version 0, run migrate_v1 (idempotent), set version
        let conn = init_db(&db_path).unwrap();
        let version = get_schema_version(&conn).unwrap();
        assert_eq!(version, SCHEMA_VERSION);

        // Existing data should still be there
        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM entries", [], |row| row.get(0))
            .unwrap();
        assert_eq!(count, 1);
    }

    #[test]
    fn test_all_tables_created() {
        let temp_dir = TempDir::new().unwrap();
        let db_path = temp_dir.path().join("test.db");

        let conn = init_db(&db_path).unwrap();

        let tables: Vec<String> = conn
            .prepare("SELECT name FROM sqlite_master WHERE type='table' ORDER BY name")
            .unwrap()
            .query_map([], |row| row.get(0))
            .unwrap()
            .filter_map(Result::ok)
            .collect();

        assert!(tables.contains(&"tags".to_string()));
        assert!(tables.contains(&"sessions".to_string()));
        assert!(tables.contains(&"entries".to_string()));
        assert!(tables.contains(&"bookmarks".to_string()));
        assert!(tables.contains(&"notes".to_string()));
        assert!(tables.contains(&"schema_version".to_string()));
    }

    #[test]
    fn test_all_entry_columns_exist() {
        let temp_dir = TempDir::new().unwrap();
        let db_path = temp_dir.path().join("test.db");

        let conn = init_db(&db_path).unwrap();

        assert!(column_exists(&conn, "entries", "tag_id"));
        assert!(column_exists(&conn, "entries", "executor_type"));
        assert!(column_exists(&conn, "entries", "executor"));
        assert!(column_exists(&conn, "sessions", "tag_id"));
    }
}
