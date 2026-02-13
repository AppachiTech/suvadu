use rusqlite::Connection;
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

/// Get the path to the suvadu database file
pub fn get_db_path() -> DbResult<PathBuf> {
    let data_dir = directories::ProjectDirs::from("tech", "appachi", "suvadu")
        .ok_or_else(|| DbError::Path("Could not determine data directory".to_string()))?
        .data_dir()
        .to_path_buf();

    std::fs::create_dir_all(&data_dir)?;
    Ok(data_dir.join("history.db"))
}

/// Initialize the database with proper schema and settings
#[allow(clippy::too_many_lines)]
pub fn init_db(path: &PathBuf) -> DbResult<Connection> {
    let conn = Connection::open(path)?;

    // Enable WAL mode for better concurrency and performance
    conn.pragma_update(None, "journal_mode", "WAL")?;
    conn.pragma_update(None, "synchronous", "NORMAL")?;

    // Create tags table
    conn.execute(
        "CREATE TABLE IF NOT EXISTS tags (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            name TEXT UNIQUE NOT NULL,
            description TEXT
        )",
        [],
    )?;

    // Create sessions table
    conn.execute(
        "CREATE TABLE IF NOT EXISTS sessions (
            id TEXT PRIMARY KEY,
            hostname TEXT NOT NULL,
            created_at INTEGER NOT NULL,
            tag_id INTEGER REFERENCES tags(id)
        )",
        [],
    )?;

    // Migration: Add tag_id to sessions if it doesn't exist (idempotent check hard in simple sqlite,
    // but ignoring error on duplicate column is a common pattern or checking pragma table_info)

    // Check if tag_id column exists
    let column_exists: bool = conn
        .query_row(
            "SELECT COUNT(*) FROM pragma_table_info('sessions') WHERE name='tag_id'",
            [],
            |row| row.get(0),
        )
        .map(|count: i64| count > 0)
        .unwrap_or(false);

    if !column_exists {
        conn.execute(
            "ALTER TABLE sessions ADD COLUMN tag_id INTEGER REFERENCES tags(id)",
            [],
        )?;
    }

    // Create entries table
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

    // Create indexes for common queries
    conn.execute(
        "CREATE INDEX IF NOT EXISTS idx_entries_session_id ON entries(session_id)",
        [],
    )?;

    conn.execute(
        "CREATE INDEX IF NOT EXISTS idx_entries_started_at ON entries(started_at)",
        [],
    )?;

    // Check if command index exists
    conn.execute(
        "CREATE INDEX IF NOT EXISTS idx_entries_command ON entries(command)",
        [],
    )?;

    // Migration: Add tag_id to entries if it doesn't exist
    let entries_tag_col_exists: bool = conn
        .query_row(
            "SELECT COUNT(*) FROM pragma_table_info('entries') WHERE name='tag_id'",
            [],
            |row| row.get(0),
        )
        .map(|count: i64| count > 0)
        .unwrap_or(false);

    if !entries_tag_col_exists {
        conn.execute(
            "ALTER TABLE entries ADD COLUMN tag_id INTEGER REFERENCES tags(id)",
            [],
        )?;

        // Create index for fast tag filtering
        conn.execute(
            "CREATE INDEX IF NOT EXISTS idx_entries_tag_id ON entries(tag_id)",
            [],
        )?;
    }

    // Migration: Add executor_type column
    let executor_type_exists: bool = conn
        .query_row(
            "SELECT COUNT(*) FROM pragma_table_info('entries') WHERE name='executor_type'",
            [],
            |row| row.get(0),
        )
        .map(|count: i64| count > 0)
        .unwrap_or(false);

    if !executor_type_exists {
        conn.execute("ALTER TABLE entries ADD COLUMN executor_type TEXT", [])?;
    }

    // Migration: Add executor column
    let executor_exists: bool = conn
        .query_row(
            "SELECT COUNT(*) FROM pragma_table_info('entries') WHERE name='executor'",
            [],
            |row| row.get(0),
        )
        .map(|count: i64| count > 0)
        .unwrap_or(false);

    if !executor_exists {
        conn.execute("ALTER TABLE entries ADD COLUMN executor TEXT", [])?;
    }

    // Create bookmarks table
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

    // Create notes table
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
}
