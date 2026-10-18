use rusqlite::{params, Connection, OptionalExtension};
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
const SCHEMA_VERSION: i64 = 10;

/// Get the path to the suvadu database file
pub fn get_db_path() -> DbResult<PathBuf> {
    let dirs = crate::util::project_dirs()
        .ok_or_else(|| DbError::Path("Could not determine data directory".to_string()))?;
    let data_dir = dirs.data_dir();

    if !data_dir.exists() {
        std::fs::create_dir_all(data_dir)?;
    }

    // Restrict directory permissions to owner-only on Unix (0o700)
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(data_dir, std::fs::Permissions::from_mode(0o700))?;
    }

    Ok(data_dir.join("history.db"))
}

/// Where backups live (`<data_dir>/backups`), without creating anything.
///
/// Read-only callers (diagnostics, storage reports) use this so that merely
/// looking at the backup directory never brings it into existence.
pub fn backup_dir_path() -> DbResult<PathBuf> {
    let dirs = crate::util::project_dirs()
        .ok_or_else(|| DbError::Path("Could not determine data directory".to_string()))?;
    Ok(dirs.data_dir().join("backups"))
}

/// Directory where database backups are written (`<data_dir>/backups`),
/// created with owner-only permissions if missing.
pub fn get_backup_dir() -> DbResult<PathBuf> {
    let dir = backup_dir_path()?;
    if !dir.exists() {
        std::fs::create_dir_all(&dir)?;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700))?;
    }
    Ok(dir)
}

/// Read the current schema version (0 if no version table exists yet).
fn get_schema_version(conn: &Connection) -> DbResult<i64> {
    conn.execute(
        "CREATE TABLE IF NOT EXISTS schema_version (version INTEGER NOT NULL)",
        [],
    )?;

    // Only "no row yet" means version 0. Any other error (a busy or damaged
    // database) must surface, not be mistaken for a fresh one and migrated.
    let version: Option<i64> = conn
        .query_row("SELECT version FROM schema_version LIMIT 1", [], |row| {
            row.get(0)
        })
        .optional()?;

    Ok(version.unwrap_or(0))
}

/// Refuse a database written by a newer Suvadu rather than downgrade it.
fn ensure_supported_version(version: i64) -> DbResult<()> {
    if version > SCHEMA_VERSION {
        return Err(DbError::Path(format!(
            "Database schema version ({version}) is newer than this version of Suvadu supports ({SCHEMA_VERSION}). \
             Please upgrade Suvadu or use the version that created this database."
        )));
    }
    Ok(())
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

/// Allowed table names for `column_exists` -- defense-in-depth against
/// SQL injection even though all callers use hardcoded literals.
const ALLOWED_TABLES: &[&str] = &[
    "entries",
    "sessions",
    "tags",
    "bookmarks",
    "notes",
    "aliases",
    "ai_summaries",
];

/// Allowed column names for `column_exists`.
const ALLOWED_COLUMNS: &[&str] = &[
    "tag_id",
    "executor_type",
    "executor",
    "description",
    "label",
    "source_event_count",
    "source_command_count",
    "source_prefix_hash",
    "base_summary_id",
];

/// Check whether a column exists on a table.
///
/// Table and column names are validated against allowlists before
/// interpolation into SQL to prevent injection (defense-in-depth).
fn column_exists(conn: &Connection, table: &str, column: &str) -> bool {
    if !ALLOWED_TABLES.contains(&table) || !ALLOWED_COLUMNS.contains(&column) {
        return false;
    }
    conn.query_row(
        &format!("SELECT COUNT(*) FROM pragma_table_info('{table}') WHERE name='{column}'"),
        [],
        |row| row.get::<_, i64>(0),
    )
    .is_ok_and(|count| count > 0)
}

/// Independent AI sessions also retain turns that execute no shell commands.
fn migrate_v8(conn: &Connection) -> DbResult<()> {
    conn.execute_batch(include_str!("ai_sessions/schema.sql"))?;
    Ok(())
}

/// Summary checkpoints remember the exact evidence prefix they cover.
fn migrate_v9(conn: &Connection) -> DbResult<()> {
    if !column_exists(conn, "ai_summaries", "source_event_count") {
        conn.execute(
            "ALTER TABLE ai_summaries ADD COLUMN source_event_count INTEGER NOT NULL DEFAULT 0",
            [],
        )?;
    }
    if !column_exists(conn, "ai_summaries", "source_command_count") {
        conn.execute(
            "ALTER TABLE ai_summaries ADD COLUMN source_command_count INTEGER NOT NULL DEFAULT 0",
            [],
        )?;
    }
    if !column_exists(conn, "ai_summaries", "source_prefix_hash") {
        conn.execute(
            "ALTER TABLE ai_summaries ADD COLUMN source_prefix_hash TEXT NOT NULL DEFAULT ''",
            [],
        )?;
    }
    if !column_exists(conn, "ai_summaries", "base_summary_id") {
        conn.execute(
            "ALTER TABLE ai_summaries ADD COLUMN base_summary_id TEXT",
            [],
        )?;
    }
    Ok(())
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
    // Note: bookmarks.command has UNIQUE constraint which creates an implicit index.
    // No separate idx_bookmarks_command needed.

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
    // Note: notes.entry_id has UNIQUE constraint which creates an implicit index.
    // No separate idx_notes_entry_id needed.

    Ok(())
}

/// Migration v2: composite indexes for stats aggregation queries.
fn migrate_v2(conn: &Connection) -> DbResult<()> {
    conn.execute_batch(
        "CREATE INDEX IF NOT EXISTS idx_entries_exit_code_started ON entries(exit_code, started_at);
         CREATE INDEX IF NOT EXISTS idx_entries_cwd_started       ON entries(cwd, started_at);
         CREATE INDEX IF NOT EXISTS idx_entries_executor_type     ON entries(executor_type);",
    )?;
    Ok(())
}

/// Migration v3: aliases table for managed shell aliases.
fn migrate_v3(conn: &Connection) -> DbResult<()> {
    conn.execute(
        "CREATE TABLE IF NOT EXISTS aliases (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            name TEXT NOT NULL UNIQUE,
            command TEXT NOT NULL,
            created_at INTEGER NOT NULL
        )",
        [],
    )?;
    // Note: aliases.name has UNIQUE constraint which creates an implicit index.
    // No separate idx_aliases_name needed.
    Ok(())
}

/// Open `path` read-only for interactive search, with the SQL functions
/// search queries call. No migrations run: the file is the one `init_db`
/// already opened in this process.
///
/// Recall runs its queries on a second connection like this one, on a worker
/// thread, so that the UI thread can keep reading keystrokes — and so that
/// interrupting a superseded query never touches the connection that writes.
pub fn open_search_reader(path: &std::path::Path) -> DbResult<Connection> {
    let flags =
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX;
    let conn = Connection::open_with_flags(path, flags)?;
    conn.busy_timeout(std::time::Duration::from_secs(5))?;
    register_regexp(&conn)?;
    register_contains_ci(&conn)?;
    register_subseq_ci(&conn)?;
    Ok(conn)
}

/// Whether `err` is `SQLite` reporting that the statement was interrupted by
/// `sqlite3_interrupt` — a query abandoned on purpose, not a failure.
pub fn is_interrupted(err: &DbError) -> bool {
    matches!(
        err,
        DbError::Sqlite(rusqlite::Error::SqliteFailure(e, _))
            if e.code == rusqlite::ErrorCode::OperationInterrupted
    )
}

/// Initialize the database with proper schema and settings.
///
/// Migrations are tracked via a `schema_version` table so each
/// migration runs exactly once. All migration functions are
/// idempotent as an extra safety net.
pub fn init_db(path: &PathBuf) -> DbResult<Connection> {
    let mut conn = Connection::open(path)?;

    // Restrict database file permissions to owner-only on Unix (0o600)
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    }

    // Retry on SQLITE_BUSY for up to 5 seconds (concurrent shell sessions)
    conn.busy_timeout(std::time::Duration::from_secs(5))?;

    // Enforce foreign key constraints (off by default in SQLite, must be set per-connection)
    conn.pragma_update(None, "foreign_keys", "ON")?;

    // Register a REGEXP function so `WHERE command REGEXP ?` works in SQL.
    // This avoids loading all rows into memory for regex-based delete/count.
    register_regexp(&conn)?;

    // Register a Unicode-aware case-insensitive substring test. SQLite's LIKE
    // only folds ASCII case, so interactive search uses this for any token
    // containing non-ASCII characters (see FilterBuilder::with_query_tokens).
    register_contains_ci(&conn)?;

    // Register the `fuzzy` mode's subsequence rule so the database can decide
    // eligibility itself. Without it, SQL could only narrow to a superset and
    // the true number of matches was unknowable without loading them all.
    register_subseq_ci(&conn)?;

    // WAL mode is persistent — only set if not already active.
    let current_mode: String = conn.pragma_query_value(None, "journal_mode", |row| row.get(0))?;
    if current_mode != "wal" {
        conn.pragma_update(None, "journal_mode", "WAL")?;
    }
    // synchronous must be set per-connection
    conn.pragma_update(None, "synchronous", "NORMAL")?;

    let version = get_schema_version(&conn)?;
    ensure_supported_version(version)?;

    // Fast path: skip migration checks if already current
    if version >= SCHEMA_VERSION {
        return Ok(conn);
    }

    run_migrations(&mut conn)?;
    Ok(conn)
}

/// Bring the schema up to `SCHEMA_VERSION`, one migration per transaction.
///
/// Several shells often start together right after an upgrade, each opening
/// the old database. Each step therefore takes the write lock first
/// (`BEGIN IMMEDIATE`, which waits out `busy_timeout` for whoever holds it)
/// and only then reads the version, so a step another process has already
/// applied is skipped rather than run twice. A deferred transaction would
/// read the version first and fail with `SQLITE_BUSY` at its first write,
/// without waiting, once another process had committed in between.
fn run_migrations(conn: &mut Connection) -> DbResult<()> {
    #[allow(clippy::type_complexity)]
    let migrations: &[(i64, fn(&Connection) -> DbResult<()>)] = &[
        (1, migrate_v1),
        (2, migrate_v2),
        (3, migrate_v3),
        (4, migrate_v4),
        (5, migrate_v5),
        (6, migrate_v6),
        (7, migrate_v7),
        (8, migrate_v8),
        (9, migrate_v9),
        (10, migrate_v10),
    ];

    loop {
        // RAII: dropping the transaction on an early return rolls it back.
        let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let applied = get_schema_version(&tx)?;
        ensure_supported_version(applied)?;
        let Some(&(target_version, migrate_fn)) =
            migrations.iter().find(|(target, _)| *target > applied)
        else {
            return Ok(());
        };
        migrate_fn(&tx)?;
        set_schema_version(&tx, target_version)?;
        tx.commit()?;
    }
}

/// Register a `REGEXP(pattern, value)` scalar function with `SQLite`.
///
/// This enables `WHERE column REGEXP ?` in SQL, letting the database engine
/// filter rows instead of loading them all into Rust for regex matching.
///
/// The compiled regex is cached in a `RefCell` so that the same pattern is only
/// compiled once per query (not once per row). `SQLite` scalar-function callbacks
/// run single-threaded, so `RefCell` is safe here.
/// Registers `suvadu_contains_ci(haystack, needle)`: true when `needle`
/// occurs in `haystack`, compared with Rust's Unicode lowercasing.
///
/// This exists because `SQLite`'s LIKE folds case for ASCII only, so
/// `command LIKE '%émile%'` never matches a command containing "Émile". It is
/// only used for tokens that actually contain non-ASCII characters; ASCII
/// tokens keep using LIKE against the trigram index, which can use that index.
fn register_contains_ci(conn: &Connection) -> DbResult<()> {
    use rusqlite::functions::FunctionFlags;

    conn.create_scalar_function(
        "suvadu_contains_ci",
        2,
        FunctionFlags::SQLITE_UTF8 | FunctionFlags::SQLITE_DETERMINISTIC,
        move |ctx| {
            let haystack = ctx
                .get_raw(0)
                .as_str_or_null()
                .unwrap_or(None)
                .unwrap_or("");
            let needle = ctx
                .get_raw(1)
                .as_str()
                .map_err(|e| rusqlite::Error::UserFunctionError(e.into()))?;
            Ok(haystack.to_lowercase().contains(&needle.to_lowercase()))
        },
    )?;
    Ok(())
}

/// Registers `suvadu_subseq_ci(haystack, needle)`: true when `needle`'s
/// characters occur in `haystack` in order, gaps allowed, folding Unicode case.
///
/// This is the `fuzzy` matching mode's documented rule, evaluated by the
/// database so that counting and paginating a fuzzy query need no in-memory
/// pass over the whole history. It shares its implementation with
/// `MatchMode::matches`, so the two can never drift apart.
fn register_subseq_ci(conn: &Connection) -> DbResult<()> {
    use rusqlite::functions::FunctionFlags;

    conn.create_scalar_function(
        "suvadu_subseq_ci",
        2,
        FunctionFlags::SQLITE_UTF8 | FunctionFlags::SQLITE_DETERMINISTIC,
        move |ctx| {
            let haystack = ctx
                .get_raw(0)
                .as_str_or_null()
                .unwrap_or(None)
                .unwrap_or("");
            let needle = ctx
                .get_raw(1)
                .as_str()
                .map_err(|e| rusqlite::Error::UserFunctionError(e.into()))?;
            Ok(crate::util::is_subsequence_ci(haystack, needle))
        },
    )?;
    Ok(())
}

fn register_regexp(conn: &Connection) -> DbResult<()> {
    use rusqlite::functions::FunctionFlags;
    use std::cell::RefCell;

    let cache: RefCell<Option<(String, regex::Regex)>> = RefCell::new(None);

    conn.create_scalar_function(
        "regexp",
        2,
        FunctionFlags::SQLITE_UTF8 | FunctionFlags::SQLITE_DETERMINISTIC,
        move |ctx| {
            let pattern = ctx
                .get_raw(0)
                .as_str()
                .map_err(|e| rusqlite::Error::UserFunctionError(e.into()))?;
            let value = ctx
                .get_raw(1)
                .as_str()
                .map_err(|e| rusqlite::Error::UserFunctionError(e.into()))?;

            let mut cached = cache.borrow_mut();
            let needs_compile = match &*cached {
                Some((p, _)) => p != pattern,
                None => true,
            };
            if needs_compile {
                let re = regex::RegexBuilder::new(pattern)
                    .size_limit(1_000_000) // 1 MB NFA limit — prevents ReDoS
                    .build()
                    .map_err(|e| rusqlite::Error::UserFunctionError(e.into()))?;
                *cached = Some((pattern.to_string(), re));
            }

            match cached.as_ref() {
                Some((_, re)) => Ok(re.is_match(value)),
                None => Err(rusqlite::Error::UserFunctionError(
                    "REGEXP: internal cache error".into(),
                )),
            }
        },
    )?;
    Ok(())
}

/// Migration v4: composite index for pattern-based deletion and counting.
fn migrate_v4(conn: &Connection) -> DbResult<()> {
    conn.execute_batch(
        "CREATE INDEX IF NOT EXISTS idx_entries_command_started ON entries(command, started_at);",
    )?;
    Ok(())
}

/// Migration v5: composite indexes for stats GROUP BY queries with date range filters.
/// `(started_at, command)` covers `WHERE started_at >= ? GROUP BY command`.
/// `(started_at, cwd)` covers `WHERE started_at >= ? GROUP BY cwd`.
fn migrate_v5(conn: &Connection) -> DbResult<()> {
    conn.execute_batch(
        "CREATE INDEX IF NOT EXISTS idx_entries_started_command ON entries(started_at, command);
         CREATE INDEX IF NOT EXISTS idx_entries_started_cwd     ON entries(started_at, cwd);",
    )?;
    Ok(())
}

/// Migration v6: `skills` table — the shared cross-agent skills library.
///
/// `(name, scope)` is unique so the same skill name can exist once globally
/// and once per project directory. `triggers` is a JSON array of strings.
fn migrate_v6(conn: &Connection) -> DbResult<()> {
    conn.execute(
        "CREATE TABLE IF NOT EXISTS skills (
            id TEXT PRIMARY KEY,
            name TEXT NOT NULL,
            description TEXT NOT NULL DEFAULT '',
            body TEXT NOT NULL,
            triggers TEXT,
            scope TEXT NOT NULL DEFAULT 'global',
            source TEXT NOT NULL DEFAULT 'human',
            status TEXT NOT NULL DEFAULT 'active',
            version INTEGER NOT NULL DEFAULT 1,
            created_at INTEGER NOT NULL,
            updated_at INTEGER NOT NULL
        )",
        [],
    )?;
    conn.execute_batch(
        "CREATE UNIQUE INDEX IF NOT EXISTS idx_skills_name_scope ON skills(name, scope);
         CREATE INDEX IF NOT EXISTS idx_skills_status ON skills(status);",
    )?;
    Ok(())
}

/// Migration v7: FTS5 trigram index over `entries.command`.
///
/// A plain `command LIKE '%text%'` scan can't use `idx_entries_command`
/// (the leading wildcard defeats a B-tree index), so it's a full table
/// scan on every keystroke of interactive search. The trigram tokenizer
/// lets a two-argument `LIKE` against `entries_fts` look its trigrams up in
/// the index instead, with the same ASCII-case-insensitive substring
/// semantics as the column scan it replaces. A `LIKE … ESCAPE` is never
/// offered to FTS5 and scans the whole index, so callers must not add one;
/// see `FilterBuilder::push_command_contains`, which also covers `%` and `_`
/// in the search text.
///
/// `entries_fts` is an "external content" table — it stores only the
/// trigram index, not a second copy of `command` — kept in sync with
/// `entries` by the three triggers below (`SQLite`'s documented pattern for
/// external-content FTS5 tables). The final `rebuild` indexes the rows that
/// existed before this migration ran. Unlike inserting them, rebuilding
/// replaces whatever the index held, so running it a second time cannot
/// index a row twice and corrupt the index.
fn migrate_v7(conn: &Connection) -> DbResult<()> {
    conn.execute(
        "CREATE VIRTUAL TABLE IF NOT EXISTS entries_fts USING fts5(
            command,
            content='entries',
            content_rowid='id',
            tokenize='trigram'
        )",
        [],
    )?;
    conn.execute_batch(
        "CREATE TRIGGER IF NOT EXISTS entries_fts_ai AFTER INSERT ON entries BEGIN
            INSERT INTO entries_fts(rowid, command) VALUES (new.id, new.command);
         END;
         CREATE TRIGGER IF NOT EXISTS entries_fts_ad AFTER DELETE ON entries BEGIN
            INSERT INTO entries_fts(entries_fts, rowid, command) VALUES ('delete', old.id, old.command);
         END;
         CREATE TRIGGER IF NOT EXISTS entries_fts_au AFTER UPDATE ON entries BEGIN
            INSERT INTO entries_fts(entries_fts, rowid, command) VALUES ('delete', old.id, old.command);
            INSERT INTO entries_fts(rowid, command) VALUES (new.id, new.command);
         END;",
    )?;
    conn.execute("INSERT INTO entries_fts(entries_fts) VALUES('rebuild')", [])?;
    Ok(())
}

/// Whether `entries_fts` holds exactly what `entries` does.
///
/// Uses FTS5's thorough `integrity-check` (`rank = 1`), which compares the
/// index against its content table; the plain check passes even when every
/// row is indexed twice, as two shells racing the old v7 backfill could
/// leave it. The check reads only — it changes nothing — so `suv doctor`
/// can run it. A mismatch is `Ok(false)`; any other failure is an error.
pub fn search_index_is_consistent(conn: &Connection) -> DbResult<bool> {
    match conn.execute(
        "INSERT INTO entries_fts(entries_fts, rank) VALUES('integrity-check', 1)",
        [],
    ) {
        Ok(_) => Ok(true),
        Err(rusqlite::Error::SqliteFailure(e, _))
            if e.code == rusqlite::ErrorCode::DatabaseCorrupt =>
        {
            Ok(false)
        }
        Err(e) => Err(e.into()),
    }
}

/// Rebuild `entries_fts` from `entries`. Search results never depend on
/// the old index contents, so this is always safe; it takes about a second
/// per 100,000 entries and holds the write lock meanwhile.
pub fn rebuild_search_index(conn: &Connection) -> DbResult<()> {
    conn.execute("INSERT INTO entries_fts(entries_fts) VALUES('rebuild')", [])?;
    Ok(())
}

/// Migration v10: re-index a row only when its indexed text can change.
///
/// v7's update trigger fired on every `UPDATE` of `entries`, so attaching
/// agent context to a command (or any other edit that leaves the command
/// alone) deleted the row from `entries_fts` and inserted it again. Firing
/// on `id` and `command` only — the two columns the index reads — keeps it
/// exactly in sync for a fraction of the writes.
fn migrate_v10(conn: &Connection) -> DbResult<()> {
    conn.execute_batch(
        "DROP TRIGGER IF EXISTS entries_fts_au;
         CREATE TRIGGER entries_fts_au AFTER UPDATE OF id, command ON entries BEGIN
            INSERT INTO entries_fts(entries_fts, rowid, command) VALUES ('delete', old.id, old.command);
            INSERT INTO entries_fts(rowid, command) VALUES (new.id, new.command);
         END;",
    )?;
    Ok(())
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
    fn test_v8_summary_table_migrates_to_checkpoint_columns() {
        let temp_dir = TempDir::new().unwrap();
        let db_path = temp_dir.path().join("test.db");
        let conn = Connection::open(&db_path).unwrap();
        conn.execute_batch(
            "CREATE TABLE schema_version (version INTEGER NOT NULL);
             INSERT INTO schema_version VALUES (8);
             -- Every v8 database has entries; v10 re-creates a trigger on it.
             CREATE TABLE entries (id INTEGER PRIMARY KEY, command TEXT NOT NULL);
             CREATE TABLE ai_summaries (
                 id TEXT PRIMARY KEY, session_id TEXT NOT NULL,
                 source_revision TEXT NOT NULL, text TEXT NOT NULL,
                 agent TEXT NOT NULL, model TEXT NOT NULL, source_ids TEXT NOT NULL,
                 created_at INTEGER NOT NULL
             );",
        )
        .unwrap();
        drop(conn);

        let conn = init_db(&db_path).unwrap();
        assert_eq!(get_schema_version(&conn).unwrap(), SCHEMA_VERSION);
        for column in [
            "source_event_count",
            "source_command_count",
            "source_prefix_hash",
            "base_summary_id",
        ] {
            assert!(column_exists(&conn, "ai_summaries", column), "{column}");
        }
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

        // The FTS5 backfill (added in migrate_v7) should have indexed the
        // pre-existing row, not just rows inserted after migration.
        let fts_count: i64 = conn
            .query_row("SELECT COUNT(*) FROM entries_fts", [], |row| row.get(0))
            .unwrap();
        assert_eq!(fts_count, 1);
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
        assert!(tables.contains(&"skills".to_string()));
        assert!(tables.contains(&"schema_version".to_string()));
        assert!(tables.contains(&"entries_fts".to_string()));
    }

    #[test]
    fn test_fts_index_stays_in_sync_with_entries() {
        let temp_dir = TempDir::new().unwrap();
        let db_path = temp_dir.path().join("test.db");
        let conn = init_db(&db_path).unwrap();

        conn.execute("INSERT INTO sessions VALUES ('s1', 'host', 1000, NULL)", [])
            .unwrap();
        let insert_entry = |cmd: &str, id: i64| {
            conn.execute(
                "INSERT INTO entries VALUES (?1, 's1', ?2, '/tmp', 0, 1000, 1100, 100, NULL, NULL, NULL, NULL)",
                params![id, cmd],
            )
            .unwrap();
        };
        insert_entry("git status", 1);
        insert_entry("cargo build --release", 2);

        // Trigger-driven insert sync: both rows are searchable immediately.
        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM entries_fts WHERE command LIKE '%git%'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(count, 1);

        // Trigger-driven delete sync: removing the row removes it from the index.
        conn.execute("DELETE FROM entries WHERE id = 1", [])
            .unwrap();
        let count_after_delete: i64 = conn
            .query_row("SELECT COUNT(*) FROM entries_fts", [], |row| row.get(0))
            .unwrap();
        assert_eq!(count_after_delete, 1);
        let git_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM entries_fts WHERE command LIKE '%git%'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(git_count, 0);
    }

    #[test]
    fn test_fts_backfill_can_run_twice_without_corrupting_the_index() {
        let temp_dir = TempDir::new().unwrap();
        let conn = init_db(&temp_dir.path().join("test.db")).unwrap();
        conn.execute("INSERT INTO sessions VALUES ('s1', 'host', 1000, NULL)", [])
            .unwrap();
        for (id, cmd) in [(1, "git status"), (2, "cargo build --release")] {
            conn.execute(
                "INSERT INTO entries VALUES (?1, 's1', ?2, '/tmp', 0, 1000, 1100, 100, NULL, NULL, NULL, NULL)",
                params![id, cmd],
            )
            .unwrap();
        }

        // As if a second process had applied v7 again over the same rows.
        migrate_v7(&conn).unwrap();

        // `rank = 1` also compares the index against `entries`; without it
        // a row indexed twice still passes.
        conn.execute_batch(
            "INSERT INTO entries_fts(entries_fts, rank) VALUES('integrity-check', 1);",
        )
        .expect("re-running the backfill must leave a consistent index");
        let hits: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM entries e WHERE e.id IN \
                 (SELECT rowid FROM entries_fts WHERE command LIKE '%release%')",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(hits, 1);
    }

    #[test]
    fn test_a_double_indexed_search_index_is_found_and_rebuilt() {
        let temp_dir = TempDir::new().unwrap();
        let conn = init_db(&temp_dir.path().join("test.db")).unwrap();
        conn.execute("INSERT INTO sessions VALUES ('s1', 'host', 1000, NULL)", [])
            .unwrap();
        for (id, cmd) in [(1, "git status"), (2, "cargo build --release")] {
            conn.execute(
                "INSERT INTO entries VALUES (?1, 's1', ?2, '/tmp', 0, 1000, 1100, 100, NULL, NULL, NULL, NULL)",
                params![id, cmd],
            )
            .unwrap();
        }
        assert!(search_index_is_consistent(&conn).unwrap());

        // What two processes racing the old v7 backfill left behind: every
        // row indexed a second time.
        conn.execute_batch(
            "INSERT INTO entries_fts(rowid, command) SELECT id, command FROM entries;",
        )
        .unwrap();
        assert!(!search_index_is_consistent(&conn).unwrap());

        rebuild_search_index(&conn).unwrap();
        assert!(search_index_is_consistent(&conn).unwrap());
        assert_eq!(fts_hits(&conn, "%release%"), 1);
        assert_eq!(fts_hits(&conn, "%status%"), 1);
    }

    /// Every block FTS5 has written for `entries_fts`, in order. Any write to
    /// the index — even a delete and re-insert of identical text — changes it.
    fn fts_blocks(conn: &Connection) -> Vec<(i64, Vec<u8>)> {
        conn.prepare("SELECT id, block FROM entries_fts_data ORDER BY id")
            .unwrap()
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap()
    }

    fn fts_hits(conn: &Connection, pattern: &str) -> i64 {
        conn.query_row(
            "SELECT COUNT(*) FROM entries e WHERE e.id IN \
             (SELECT rowid FROM entries_fts WHERE command LIKE ?1)",
            params![pattern],
            |row| row.get(0),
        )
        .unwrap()
    }

    #[test]
    fn test_only_a_command_change_rewrites_the_search_index() {
        let temp_dir = TempDir::new().unwrap();
        let conn = init_db(&temp_dir.path().join("test.db")).unwrap();
        conn.execute("INSERT INTO sessions VALUES ('s1', 'host', 1000, NULL)", [])
            .unwrap();
        conn.execute(
            "INSERT INTO entries VALUES (1, 's1', 'git status', '/tmp', 0, 1000, 1100, 100, NULL, NULL, NULL, NULL)",
            [],
        )
        .unwrap();

        // Attaching agent context (and other non-command edits) must not
        // touch the index at all.
        let before = fts_blocks(&conn);
        conn.execute(
            "UPDATE entries SET context = '{\"agent\":\"x\"}', exit_code = 1, cwd = '/srv' WHERE id = 1",
            [],
        )
        .unwrap();
        assert_eq!(
            fts_blocks(&conn),
            before,
            "a non-command update re-indexed the row"
        );

        // Changing the command re-indexes it: old text gone, new text found.
        conn.execute(
            "UPDATE entries SET command = 'cargo build' WHERE id = 1",
            [],
        )
        .unwrap();
        assert_ne!(fts_blocks(&conn), before);
        assert_eq!(fts_hits(&conn, "%status%"), 0);
        assert_eq!(fts_hits(&conn, "%build%"), 1);
        conn.execute_batch(
            "INSERT INTO entries_fts(entries_fts, rank) VALUES('integrity-check', 1);",
        )
        .unwrap();
    }

    #[test]
    fn test_an_unreadable_schema_version_is_an_error_not_version_zero() {
        // Reading as version 0 would re-run every migration over the data.
        let temp_dir = TempDir::new().unwrap();
        let conn = Connection::open(temp_dir.path().join("test.db")).unwrap();
        conn.execute_batch(
            "CREATE TABLE schema_version (v INTEGER); INSERT INTO schema_version VALUES (9);",
        )
        .unwrap();
        assert!(get_schema_version(&conn).is_err());

        // An empty table is the one case that does mean "nothing applied".
        conn.execute_batch("DROP TABLE schema_version").unwrap();
        assert_eq!(get_schema_version(&conn).unwrap(), 0);
    }

    #[test]
    fn test_skills_name_scope_unique() {
        let temp_dir = TempDir::new().unwrap();
        let db_path = temp_dir.path().join("test.db");
        let conn = init_db(&db_path).unwrap();

        conn.execute(
            "INSERT INTO skills (id, name, body, scope, created_at, updated_at)
             VALUES ('a', 'deploy', 'body', 'global', 1, 1)",
            [],
        )
        .unwrap();

        // Same name + same scope should violate the unique index.
        let dup = conn.execute(
            "INSERT INTO skills (id, name, body, scope, created_at, updated_at)
             VALUES ('b', 'deploy', 'other', 'global', 2, 2)",
            [],
        );
        assert!(dup.is_err());

        // Same name, different scope is fine.
        let ok = conn.execute(
            "INSERT INTO skills (id, name, body, scope, created_at, updated_at)
             VALUES ('c', 'deploy', 'other', '/tmp/project', 3, 3)",
            [],
        );
        assert!(ok.is_ok());
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

    #[test]
    fn test_column_exists_rejects_unlisted_table() {
        let temp_dir = TempDir::new().unwrap();
        let db_path = temp_dir.path().join("test.db");
        let conn = init_db(&db_path).unwrap();

        assert!(!column_exists(&conn, "evil_table", "tag_id"));
    }

    #[test]
    fn test_column_exists_rejects_unlisted_column() {
        let temp_dir = TempDir::new().unwrap();
        let db_path = temp_dir.path().join("test.db");
        let conn = init_db(&db_path).unwrap();

        assert!(!column_exists(&conn, "entries", "evil_column"));
    }

    #[test]
    fn test_foreign_keys_enforced() {
        let temp_dir = TempDir::new().unwrap();
        let db_path = temp_dir.path().join("test.db");
        let conn = init_db(&db_path).unwrap();

        // Verify PRAGMA foreign_keys is ON
        let fk_enabled: bool = conn
            .pragma_query_value(None, "foreign_keys", |row| row.get(0))
            .unwrap();
        assert!(fk_enabled, "foreign_keys pragma should be ON");

        // Inserting an entry referencing a non-existent session should fail
        let result = conn.execute(
            "INSERT INTO entries (session_id, command, cwd, exit_code, started_at, ended_at, duration_ms)
             VALUES ('nonexistent', 'ls', '/tmp', 0, 1000, 1100, 100)",
            [],
        );
        assert!(result.is_err(), "FK violation should be rejected");
    }

    #[test]
    #[cfg(unix)]
    fn test_db_file_permissions() {
        use std::os::unix::fs::PermissionsExt;

        let temp_dir = TempDir::new().unwrap();
        let db_path = temp_dir.path().join("test.db");
        let _conn = init_db(&db_path).unwrap();

        let mode = std::fs::metadata(&db_path).unwrap().permissions().mode() & 0o777;
        assert_eq!(
            mode, 0o600,
            "DB file should be owner-only (0o600), got {mode:o}"
        );
    }

    #[test]
    fn test_column_exists_rejects_sql_injection_attempt() {
        let temp_dir = TempDir::new().unwrap();
        let db_path = temp_dir.path().join("test.db");
        let conn = init_db(&db_path).unwrap();

        assert!(!column_exists(
            &conn,
            "entries'; DROP TABLE entries; --",
            "tag_id"
        ));
        assert!(!column_exists(&conn, "entries", "tag_id' OR '1'='1"));
    }
}
