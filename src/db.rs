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
const SCHEMA_VERSION: i64 = 15;

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
        (11, migrate_v11),
        (12, migrate_v12),
        (13, migrate_v13),
        (14, migrate_v14),
        (15, migrate_v15),
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
/// occurs in `haystack`, both case-folded ([`crate::util::fold_case`]), so
/// `σ` finds `ΟΔΟΣ` and `straße` finds `STRASSE`.
///
/// This exists because `SQLite`'s LIKE folds case for ASCII only, so
/// `command LIKE '%émile%'` never matches a command containing "Émile". It is
/// only used for tokens that actually contain non-ASCII characters; ASCII
/// tokens keep using LIKE against the trigram index, which can use that index.
fn register_contains_ci(conn: &Connection) -> DbResult<()> {
    use rusqlite::functions::FunctionFlags;
    use std::cell::RefCell;

    // The needle is the same on every row of a query: fold it once. Scalar
    // functions run on one thread at a time, so a `RefCell` is enough.
    let folded: RefCell<Option<(String, String)>> = RefCell::new(None);

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
            let mut folded = folded.borrow_mut();
            if folded.as_ref().is_none_or(|(raw, _)| raw != needle) {
                *folded = Some((needle.to_string(), crate::util::fold_case(needle)));
            }
            let needle = folded.as_ref().map_or("", |(_, f)| f.as_str());
            Ok(crate::util::contains_folded(haystack, needle))
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

/// The full-text indexes over `entries.command`: the plain one (v7) and the
/// separator-insensitive one (v11). Both are external-content FTS5 tables,
/// checked and rebuilt the same way.
const SEARCH_INDEXES: [&str; 2] = ["entries_fts", "entries_norm_fts"];

/// Whether every search index holds exactly what `entries` does.
///
/// Uses FTS5's thorough `integrity-check` (`rank = 1`), which compares an
/// index against its content; the plain check passes even when every row is
/// indexed twice, as two shells racing the old v7 backfill could leave it.
/// The check reads only — it changes nothing — so `suv doctor` can run it.
/// A mismatch in either index is `Ok(false)`; any other failure is an error.
pub fn search_index_is_consistent(conn: &Connection) -> DbResult<bool> {
    for index in SEARCH_INDEXES {
        match conn.execute(
            &format!("INSERT INTO {index}({index}, rank) VALUES('integrity-check', 1)"),
            [],
        ) {
            Ok(_) => {}
            Err(rusqlite::Error::SqliteFailure(e, _))
                if e.code == rusqlite::ErrorCode::DatabaseCorrupt =>
            {
                return Ok(false);
            }
            Err(e) => return Err(e.into()),
        }
    }
    Ok(true)
}

/// Rebuild every search index from `entries`. Search results never depend
/// on the old index contents, so this is always safe; it holds the write
/// lock meanwhile, about one and a half seconds per 100,000 entries.
pub fn rebuild_search_index(conn: &Connection) -> DbResult<()> {
    for index in SEARCH_INDEXES {
        conn.execute(
            &format!("INSERT INTO {index}({index}) VALUES('rebuild')"),
            [],
        )?;
    }
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

/// `expr` (an SQL text expression) in the separator-insensitive form the
/// `entries_norm_fts` index holds: every space, tab, newline, `-`, `_`, `.`,
/// `/` and `:` ([`crate::util::SEPARATORS`]) removed, then ASCII letters
/// lower-cased.
///
/// Plain `replace()` and `lower()` rather than a registered function, so the
/// view, its triggers and the index stay usable from the `sqlite3` shell or a
/// restored backup, where no Suvadu function exists. `lower()` folds ASCII
/// only; [`crate::util::normalize_separators`] does the same, and a test holds
/// the two to one answer.
///
/// This text is part of schema v11: the triggers and the view store it. A
/// change to it, or to the separator list, needs a migration that recreates
/// them and rebuilds the index.
pub fn normalized_sql(expr: &str) -> String {
    format!(
        "lower(replace(replace(replace(replace(replace(replace(replace(replace({expr}, \
         ' ', ''), char(9), ''), char(10), ''), '-', ''), '_', ''), '.', ''), '/', ''), ':', ''))"
    )
}

/// Migration v11: a second trigram index, over each command with its
/// separators removed (see [`normalized_sql`]).
///
/// It answers recall's separator-insensitive fallback — `hiveagent` finding
/// `hive-agent` — through the index, the way `entries_fts` answers ordinary
/// substring search. `entries_norm_v` computes the normalized text and is the
/// index's external content, so nothing is stored twice; the triggers mirror
/// `entries_fts`'s (with v10's narrower update trigger), handing FTS5 the
/// normalized old and new text. `rebuild` indexes existing history and, like
/// v7's, cannot index a row twice if another process already ran it.
fn migrate_v11(conn: &Connection) -> DbResult<()> {
    let new = normalized_sql("new.command");
    let old = normalized_sql("old.command");
    conn.execute_batch(&format!(
        "CREATE VIEW IF NOT EXISTS entries_norm_v(id, command_norm) AS
            SELECT id, {command} FROM entries;
         CREATE VIRTUAL TABLE IF NOT EXISTS entries_norm_fts USING fts5(
            command_norm,
            content='entries_norm_v',
            content_rowid='id',
            tokenize='trigram'
         );
         CREATE TRIGGER IF NOT EXISTS entries_norm_fts_ai AFTER INSERT ON entries BEGIN
            INSERT INTO entries_norm_fts(rowid, command_norm) VALUES (new.id, {new});
         END;
         CREATE TRIGGER IF NOT EXISTS entries_norm_fts_ad AFTER DELETE ON entries BEGIN
            INSERT INTO entries_norm_fts(entries_norm_fts, rowid, command_norm)
                VALUES ('delete', old.id, {old});
         END;
         CREATE TRIGGER IF NOT EXISTS entries_norm_fts_au AFTER UPDATE OF id, command ON entries BEGIN
            INSERT INTO entries_norm_fts(entries_norm_fts, rowid, command_norm)
                VALUES ('delete', old.id, {old});
            INSERT INTO entries_norm_fts(rowid, command_norm) VALUES (new.id, {new});
         END;
         INSERT INTO entries_norm_fts(entries_norm_fts) VALUES('rebuild');",
        command = normalized_sql("command"),
    ))?;
    Ok(())
}

/// Migration v12: an index that answers a prefix search by seeking.
///
/// A prefix search (`command LIKE 'git%'`: recall's `prefix` mode, and
/// `suv get --prefix`, which Up-arrow and the zsh-autosuggestions strategy
/// run on every keystroke) folds ASCII case, so `idx_entries_command`, in
/// binary order, cannot narrow it: `Git…` and `git…` are not one range, and
/// every lookup read the whole table. With the index in `NOCASE` order —
/// which folds exactly the case `LIKE` folds — `SQLite`'s LIKE optimization
/// turns the prefix into one range of it. `started_at`, `executor_type` and
/// `cwd` ride along so that choosing Up-arrow's command (newest first, agent
/// commands hidden, the current directory breaking ties) reads the index
/// alone.
///
/// `idx_entries_command` goes: `idx_entries_command_started` starts with the
/// same column and answers every lookup it did, so it only cost space.
fn migrate_v12(conn: &Connection) -> DbResult<()> {
    conn.execute_batch(
        "CREATE INDEX IF NOT EXISTS idx_entries_command_nocase
            ON entries(command COLLATE NOCASE, started_at, executor_type, cwd);
         DROP INDEX IF EXISTS idx_entries_command;",
    )?;
    Ok(())
}

/// Migration v13: Smart rank's statistics come from an index alone.
///
/// Recall's Smart rank sums a weight per run by age and counts failures for
/// every distinct command in its ranked window, leaving agent runs out
/// while they are hidden (`Repository::command_stats`). The command index
/// found those runs but held neither their exit status nor their executor,
/// so each run cost a lookup in the table, which dominated a one-letter
/// query on a large history (BENCHMARKS.md). With `exit_code` and
/// `executor_type` in the index it is read alone. The index keeps its name and its leading
/// columns, so every lookup it answered before it still answers.
fn migrate_v13(conn: &Connection) -> DbResult<()> {
    conn.execute_batch(
        "DROP INDEX IF EXISTS idx_entries_command_started;
         CREATE INDEX idx_entries_command_started
            ON entries(command, started_at, exit_code, executor_type);",
    )?;
    Ok(())
}

/// Migration v14: `entry_output`, the end of what a failed command printed.
///
/// Off by default (`[capture] failure_output`), and filled only by the
/// paths that see a command's output: agent hooks and `suv wrap`. A table
/// of its own rather than a key in `entries.context`, so that recall and
/// the indexes never read a few kilobytes of text per row they do not
/// show, every tool that prints a command's context does not start
/// printing its output, and the rows go with their command through the
/// foreign key. It starts empty, so upgrading copies nothing.
fn migrate_v14(conn: &Connection) -> DbResult<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS entry_output (
            entry_id INTEGER PRIMARY KEY REFERENCES entries(id) ON DELETE CASCADE,
            stream TEXT NOT NULL,
            tail TEXT NOT NULL,
            truncated_bytes INTEGER NOT NULL DEFAULT 0
         );",
    )?;
    Ok(())
}

/// Migration v15: `ai_snapshots`, git snapshots of an agent session's
/// working directory.
///
/// At most two rows per session — the snapshot at its first prompt and the
/// latest turn end's — holding the commit and branch checked out and the
/// paths `git status` reported changed (never contents, never a diff). It
/// is keyed by the session id commands and transcripts use, with no
/// foreign key, because the first snapshot is taken before the session has
/// any other record; deleting an agent session deletes its snapshots, and
/// `suv gc` removes those left without a session. It starts empty, so
/// upgrading copies nothing.
fn migrate_v15(conn: &Connection) -> DbResult<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS ai_snapshots (
            session_id TEXT NOT NULL,
            kind TEXT NOT NULL CHECK (kind IN ('start', 'end')),
            at INTEGER NOT NULL,
            cwd TEXT NOT NULL,
            repo_root TEXT NOT NULL,
            head TEXT,
            branch TEXT,
            paths TEXT NOT NULL,
            path_count INTEGER NOT NULL,
            omitted INTEGER NOT NULL DEFAULT 0,
            capped INTEGER NOT NULL DEFAULT 0,
            PRIMARY KEY (session_id, kind)
         );
         CREATE INDEX IF NOT EXISTS ai_snapshots_at ON ai_snapshots(kind, at);",
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
             -- Every v8 database has entries; v10 re-creates a trigger on it
             -- and v12 and v13 index these columns.
             CREATE TABLE entries (
                 id INTEGER PRIMARY KEY, command TEXT NOT NULL, cwd TEXT NOT NULL,
                 exit_code INTEGER, started_at INTEGER NOT NULL, executor_type TEXT
             );
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
        assert!(tables.contains(&"entry_output".to_string()));
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

    fn insert_entry(conn: &Connection, id: i64, command: &str) {
        conn.execute(
            "INSERT INTO entries VALUES (?1, 's1', ?2, '/tmp', 0, 1000, 1100, 100, NULL, NULL, NULL, NULL)",
            params![id, command],
        )
        .unwrap();
    }

    /// Entries whose separator-insensitive form matches `pattern` (a
    /// two-argument LIKE, as search asks it).
    fn norm_hits(conn: &Connection, pattern: &str) -> Vec<i64> {
        conn.prepare(
            "SELECT e.id FROM entries e WHERE e.id IN \
             (SELECT rowid FROM entries_norm_fts WHERE command_norm LIKE ?1) ORDER BY e.id",
        )
        .unwrap()
        .query_map(params![pattern], |row| row.get(0))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap()
    }

    fn norm_index_is_intact(conn: &Connection) -> bool {
        conn.execute_batch(
            "INSERT INTO entries_norm_fts(entries_norm_fts, rank) VALUES('integrity-check', 1);",
        )
        .is_ok()
    }

    #[test]
    fn test_the_separator_index_expression_drops_separators_and_folds_ascii() {
        let conn = Connection::open_in_memory().unwrap();
        let sql = format!("SELECT {}", normalized_sql("?1"));
        for (sample, expected) in [
            ("hive-agent", "hiveagent"),
            ("Hive_Agent.v2/run:now", "hiveagentv2runnow"),
            (
                "kubectl  get\tpods\n-n kube-system",
                "kubectlgetpodsnkubesystem",
            ),
            ("C:\\Temp\\x", "c\\temp\\x"),
            ("50% done_", "50%done"),
            // lower() folds ASCII only.
            ("ÉCHO-Émile", "ÉchoÉmile"),
            ("line\r\nbreak", "line\rbreak"),
            ("--._/: \t\n", ""),
        ] {
            let from_sql: String = conn
                .query_row(&sql, params![sample], |row| row.get(0))
                .unwrap();
            assert_eq!(from_sql, expected, "{sample:?}");
        }
    }

    #[test]
    fn test_the_separator_index_expression_is_normalize_separators() {
        // The index is built by SQL and queried with terms normalized in
        // Rust; if the two ever disagreed, a match could be missed.
        let conn = Connection::open_in_memory().unwrap();
        let sql = format!("SELECT {}", normalized_sql("?1"));
        for sample in [
            "hive-agent",
            "Hive_Agent.v2/run:now",
            "kubectl  get\tpods\n-n kube-system",
            "C:\\Temp\\x",
            "50% done_",
            "ÉCHO-Émile ΣΟΦΙΑ \u{212a}elvin İstanbul",
            "line\r\nbreak\u{a0}nbsp\u{3000}ideographic",
            "",
            "--._/: \t\n",
        ] {
            let from_sql: String = conn
                .query_row(&sql, params![sample], |row| row.get(0))
                .unwrap();
            assert_eq!(
                from_sql,
                crate::util::normalize_separators(sample),
                "{sample:?}"
            );
        }
    }

    #[test]
    fn test_v11_indexes_the_history_recorded_before_it() {
        let temp_dir = TempDir::new().unwrap();
        let db_path = temp_dir.path().join("test.db");
        let conn = init_db(&db_path).unwrap();
        conn.execute("INSERT INTO sessions VALUES ('s1', 'host', 1000, NULL)", [])
            .unwrap();
        // Back to a v10 database: none of v11's objects, and rows written
        // while they did not exist.
        conn.execute_batch(
            "DROP TRIGGER IF EXISTS entries_norm_fts_ai;
             DROP TRIGGER IF EXISTS entries_norm_fts_ad;
             DROP TRIGGER IF EXISTS entries_norm_fts_au;
             DROP TABLE IF EXISTS entries_norm_fts;
             DROP VIEW IF EXISTS entries_norm_v;
             UPDATE schema_version SET version = 10;",
        )
        .unwrap();
        insert_entry(&conn, 1, "ssh hive-agent.internal");
        insert_entry(&conn, 2, "git status");
        drop(conn);

        let conn = init_db(&db_path).unwrap();
        assert_eq!(get_schema_version(&conn).unwrap(), SCHEMA_VERSION);
        assert_eq!(norm_hits(&conn, "%hiveagentinternal%"), vec![1]);
        assert_eq!(norm_hits(&conn, "%gitstatus%"), vec![2]);
        assert!(norm_index_is_intact(&conn));

        // Running the migration again (another shell got there first) must
        // not index anything twice.
        migrate_v11(&conn).unwrap();
        assert!(norm_index_is_intact(&conn));
        assert_eq!(norm_hits(&conn, "%hiveagent%"), vec![1]);
    }

    #[test]
    fn test_the_separator_index_follows_inserts_updates_and_deletes() {
        let temp_dir = TempDir::new().unwrap();
        let conn = init_db(&temp_dir.path().join("test.db")).unwrap();
        conn.execute("INSERT INTO sessions VALUES ('s1', 'host', 1000, NULL)", [])
            .unwrap();
        insert_entry(&conn, 1, "hive-agent start");
        insert_entry(&conn, 2, "hive_agent stop");
        assert_eq!(norm_hits(&conn, "%hiveagent%"), vec![1, 2]);

        // A non-command edit must not touch the index at all.
        let blocks = |conn: &Connection| -> Vec<(i64, Vec<u8>)> {
            conn.prepare("SELECT id, block FROM entries_norm_fts_data ORDER BY id")
                .unwrap()
                .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
                .unwrap()
                .collect::<Result<_, _>>()
                .unwrap()
        };
        let before = blocks(&conn);
        conn.execute(
            "UPDATE entries SET cwd = '/srv', exit_code = 1 WHERE id = 1",
            [],
        )
        .unwrap();
        assert_eq!(blocks(&conn), before, "a non-command update re-indexed");

        conn.execute(
            "UPDATE entries SET command = 'cargo build' WHERE id = 1",
            [],
        )
        .unwrap();
        assert_eq!(norm_hits(&conn, "%hiveagent%"), vec![2]);
        assert_eq!(norm_hits(&conn, "%cargobuild%"), vec![1]);

        conn.execute("DELETE FROM entries WHERE id = 2", [])
            .unwrap();
        assert!(norm_hits(&conn, "%hiveagent%").is_empty());
        assert!(norm_index_is_intact(&conn));
    }

    #[test]
    fn test_the_consistency_check_covers_the_separator_index() {
        let temp_dir = TempDir::new().unwrap();
        let conn = init_db(&temp_dir.path().join("test.db")).unwrap();
        conn.execute("INSERT INTO sessions VALUES ('s1', 'host', 1000, NULL)", [])
            .unwrap();
        insert_entry(&conn, 1, "hive-agent start");
        assert!(search_index_is_consistent(&conn).unwrap());

        // Only the separator index drifts; the check must still notice.
        conn.execute_batch(
            "INSERT INTO entries_norm_fts(rowid, command_norm) SELECT id, command_norm FROM entries_norm_v;",
        )
        .unwrap();
        assert!(!search_index_is_consistent(&conn).unwrap());

        rebuild_search_index(&conn).unwrap();
        assert!(search_index_is_consistent(&conn).unwrap());
        assert_eq!(norm_hits(&conn, "%hiveagent%"), vec![1]);
    }

    /// How long v11 holds the write lock on a large history, and what its
    /// index costs on disk. Prints rather than asserts: both depend on the
    /// machine and the corpus. Run with
    /// `cargo test --release --bin suv v11_migration_cost -- --ignored --nocapture`.
    #[test]
    #[ignore = "builds a 100k-entry database; run explicitly with --ignored"]
    fn v11_migration_cost_on_a_large_history() {
        let temp_dir = TempDir::new().unwrap();
        let db_path = temp_dir.path().join("test.db");
        let mut conn = init_db(&db_path).unwrap();
        conn.execute_batch(
            "DROP TRIGGER entries_norm_fts_ai; DROP TRIGGER entries_norm_fts_ad;
             DROP TRIGGER entries_norm_fts_au; DROP TABLE entries_norm_fts;
             DROP VIEW entries_norm_v; UPDATE schema_version SET version = 10;
             INSERT INTO sessions VALUES ('s1', 'host', 1000, NULL);",
        )
        .unwrap();
        let shapes = [
            "cargo test --workspace --package suvadu-core case_",
            "kubectl get pods -n kube-system -l app=api-gateway-",
            "git commit -m 'fix: handle the hive-agent reconnect path' #",
            "docker compose -f deploy/docker-compose.prod.yml up -d svc_",
            "ssh deploy@build-runner-",
            "rg --hidden -g '!target' 'normalize_separators' src/",
        ];
        conn.execute_batch("BEGIN").unwrap();
        for i in 0..100_000_i64 {
            let shape = shapes[usize::try_from(i).unwrap() % shapes.len()];
            insert_entry(&conn, i + 1, &format!("{shape}{i}"));
        }
        conn.execute_batch("COMMIT").unwrap();
        let bytes = |conn: &Connection| -> i64 {
            conn.query_row(
                "SELECT page_count * page_size FROM pragma_page_count, pragma_page_size",
                [],
                |row| row.get(0),
            )
            .unwrap()
        };
        let before = bytes(&conn);
        let start = std::time::Instant::now();
        run_migrations(&mut conn).unwrap();
        let migrated = start.elapsed();
        let after = bytes(&conn);
        let start = std::time::Instant::now();
        rebuild_search_index(&conn).unwrap();
        let rebuilt = start.elapsed();
        println!(
            "v11 at 100k entries: migration {migrated:?}, full rebuild {rebuilt:?}, \
             database {} KiB -> {} KiB",
            before / 1024,
            after / 1024
        );
    }

    fn index_names(conn: &Connection) -> Vec<String> {
        let mut stmt = conn
            .prepare("SELECT name FROM sqlite_master WHERE type = 'index' AND tbl_name = 'entries'")
            .unwrap();
        stmt.query_map([], |row| row.get(0))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap()
    }

    /// Back to a v11 database: `idx_entries_command`, and no v12 index.
    fn revert_to_v11(conn: &Connection) {
        conn.execute_batch(
            "DROP INDEX IF EXISTS idx_entries_command_nocase;
             CREATE INDEX IF NOT EXISTS idx_entries_command ON entries(command);
             UPDATE schema_version SET version = 11;",
        )
        .unwrap();
    }

    #[test]
    fn test_v12_replaces_the_binary_command_index_with_a_case_folding_one() {
        let temp_dir = TempDir::new().unwrap();
        let db_path = temp_dir.path().join("test.db");
        let conn = init_db(&db_path).unwrap();
        conn.execute("INSERT INTO sessions VALUES ('s1', 'host', 1000, NULL)", [])
            .unwrap();
        revert_to_v11(&conn);
        insert_entry(&conn, 1, "Git status");
        insert_entry(&conn, 2, "git log");
        insert_entry(&conn, 3, "gitk");
        drop(conn);

        let conn = init_db(&db_path).unwrap();
        assert_eq!(get_schema_version(&conn).unwrap(), SCHEMA_VERSION);
        let names = index_names(&conn);
        assert!(
            names.contains(&"idx_entries_command_nocase".to_string()),
            "{names:?}"
        );
        assert!(
            !names.contains(&"idx_entries_command".to_string()),
            "{names:?}"
        );
        // Still the command lookups' index.
        assert!(
            names.contains(&"idx_entries_command_started".to_string()),
            "{names:?}"
        );
        let hits: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM entries INDEXED BY idx_entries_command_nocase
                 WHERE command LIKE 'GIT %'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(hits, 2);
        // Another shell running it again changes nothing.
        migrate_v12(&conn).unwrap();
        assert_eq!(index_names(&conn), names);
    }

    /// How long v12 holds the write lock on a large history, and what its
    /// index costs on disk. Prints rather than asserts. Run with
    /// `cargo test --release --bin suv v12_migration_cost -- --ignored --nocapture`.
    #[test]
    #[ignore = "builds a 100k-entry database; run explicitly with --ignored"]
    fn v12_migration_cost_on_a_large_history() {
        let temp_dir = TempDir::new().unwrap();
        let db_path = temp_dir.path().join("test.db");
        let mut conn = init_db(&db_path).unwrap();
        conn.execute("INSERT INTO sessions VALUES ('s1', 'host', 1000, NULL)", [])
            .unwrap();
        revert_to_v11(&conn);
        let shapes = [
            "cargo test --workspace --package suvadu-core case_",
            "kubectl get pods -n kube-system -l app=api-gateway-",
            "git commit -m 'fix: handle the hive-agent reconnect path' #",
            "docker compose -f deploy/docker-compose.prod.yml up -d svc_",
            "ssh deploy@build-runner-",
            "rg --hidden -g '!target' 'normalize_separators' src/",
        ];
        conn.execute_batch("BEGIN").unwrap();
        for i in 0..100_000_i64 {
            let shape = shapes[usize::try_from(i).unwrap() % shapes.len()];
            insert_entry(&conn, i + 1, &format!("{shape}{i}"));
        }
        conn.execute_batch("COMMIT").unwrap();
        conn.execute_batch("VACUUM").unwrap();
        let bytes = |conn: &Connection| -> i64 {
            conn.query_row(
                "SELECT page_count * page_size FROM pragma_page_count, pragma_page_size",
                [],
                |row| row.get(0),
            )
            .unwrap()
        };
        let before = bytes(&conn);
        let start = std::time::Instant::now();
        run_migrations(&mut conn).unwrap();
        let migrated = start.elapsed();
        conn.execute_batch("VACUUM").unwrap();
        let after = bytes(&conn);
        println!(
            "v12 at 100k entries: migration {migrated:?}, database {} KiB -> {} KiB",
            before / 1024,
            after / 1024
        );
    }

    /// The columns of index `name`, in order.
    fn index_columns(conn: &Connection, name: &str) -> Vec<String> {
        let mut stmt = conn
            .prepare("SELECT name FROM pragma_index_info(?1) ORDER BY seqno")
            .unwrap();
        stmt.query_map([name], |row| row.get(0))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap()
    }

    /// Back to a v12 database: the command index without the outcome
    /// columns.
    fn revert_to_v12(conn: &Connection) {
        conn.execute_batch(
            "DROP INDEX IF EXISTS idx_entries_command_started;
             CREATE INDEX idx_entries_command_started ON entries(command, started_at);
             UPDATE schema_version SET version = 12;",
        )
        .unwrap();
    }

    #[test]
    fn test_v13_carries_outcomes_in_the_command_index() {
        let temp_dir = TempDir::new().unwrap();
        let db_path = temp_dir.path().join("test.db");
        let conn = init_db(&db_path).unwrap();
        conn.execute("INSERT INTO sessions VALUES ('s1', 'host', 1000, NULL)", [])
            .unwrap();
        revert_to_v12(&conn);
        insert_entry(&conn, 1, "make");
        insert_entry(&conn, 2, "make");
        drop(conn);

        let conn = init_db(&db_path).unwrap();
        assert_eq!(get_schema_version(&conn).unwrap(), SCHEMA_VERSION);
        assert_eq!(
            index_columns(&conn, "idx_entries_command_started"),
            ["command", "started_at", "exit_code", "executor_type"]
        );
        let runs: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM entries INDEXED BY idx_entries_command_started
                 WHERE command = 'make'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(runs, 2);
        // Another shell running it again changes nothing.
        let names = index_names(&conn);
        migrate_v13(&conn).unwrap();
        assert_eq!(index_names(&conn), names);
        assert_eq!(index_columns(&conn, "idx_entries_command_started").len(), 4);
    }

    /// How long v13 holds the write lock on a large history, and what its
    /// wider index costs on disk. Prints rather than asserts. Run with
    /// `cargo test --release --bin suv v13_migration_cost -- --ignored --nocapture`.
    #[test]
    #[ignore = "builds a 100k-entry database; run explicitly with --ignored"]
    fn v13_migration_cost_on_a_large_history() {
        let temp_dir = TempDir::new().unwrap();
        let db_path = temp_dir.path().join("test.db");
        let mut conn = init_db(&db_path).unwrap();
        conn.execute("INSERT INTO sessions VALUES ('s1', 'host', 1000, NULL)", [])
            .unwrap();
        revert_to_v12(&conn);
        let shapes = [
            "cargo test --workspace --package suvadu-core case_",
            "kubectl get pods -n kube-system -l app=api-gateway-",
            "git commit -m 'fix: handle the hive-agent reconnect path' #",
            "docker compose -f deploy/docker-compose.prod.yml up -d svc_",
            "ssh deploy@build-runner-",
            "rg --hidden -g '!target' 'normalize_separators' src/",
        ];
        conn.execute_batch("BEGIN").unwrap();
        for i in 0..100_000_i64 {
            let shape = shapes[usize::try_from(i).unwrap() % shapes.len()];
            insert_entry(&conn, i + 1, &format!("{shape}{}", i % 1000));
        }
        conn.execute_batch("COMMIT").unwrap();
        conn.execute_batch("VACUUM").unwrap();
        let bytes = |conn: &Connection| -> i64 {
            conn.query_row(
                "SELECT page_count * page_size FROM pragma_page_count, pragma_page_size",
                [],
                |row| row.get(0),
            )
            .unwrap()
        };
        let before = bytes(&conn);
        let start = std::time::Instant::now();
        run_migrations(&mut conn).unwrap();
        let migrated = start.elapsed();
        conn.execute_batch("VACUUM").unwrap();
        let after = bytes(&conn);
        println!(
            "v13 at 100k entries: migration {migrated:?}, database {} KiB -> {} KiB",
            before / 1024,
            after / 1024
        );
    }

    /// Back to a v13 database: no `entry_output` table.
    fn revert_to_v13(conn: &Connection) {
        conn.execute_batch(
            "DROP TABLE IF EXISTS entry_output;
             UPDATE schema_version SET version = 13;",
        )
        .unwrap();
    }

    #[test]
    fn test_v14_adds_an_output_table_that_goes_with_its_command() {
        let temp_dir = TempDir::new().unwrap();
        let db_path = temp_dir.path().join("test.db");
        let conn = init_db(&db_path).unwrap();
        conn.execute("INSERT INTO sessions VALUES ('s1', 'host', 1000, NULL)", [])
            .unwrap();
        revert_to_v13(&conn);
        insert_entry(&conn, 1, "make");
        insert_entry(&conn, 2, "make test");
        drop(conn);

        let conn = init_db(&db_path).unwrap();
        assert_eq!(get_schema_version(&conn).unwrap(), SCHEMA_VERSION);
        for id in [1, 2] {
            conn.execute(
                "INSERT INTO entry_output (entry_id, stream, tail, truncated_bytes)
                 VALUES (?1, 'stderr', 'error: no rule', 0)",
                [id],
            )
            .unwrap();
        }
        // A tail belongs to a command that exists...
        assert!(conn
            .execute(
                "INSERT INTO entry_output (entry_id, stream, tail) VALUES (99, 'stderr', 'x')",
                [],
            )
            .is_err());
        // ...and goes when the command does.
        conn.execute("DELETE FROM entries WHERE id = 1", [])
            .unwrap();
        let left: Vec<i64> = conn
            .prepare("SELECT entry_id FROM entry_output")
            .unwrap()
            .query_map([], |row| row.get(0))
            .unwrap()
            .map(Result::unwrap)
            .collect();
        assert_eq!(left, [2]);
        // Another shell running it again changes nothing.
        migrate_v14(&conn).unwrap();
        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM entry_output", [], |row| row.get(0))
            .unwrap();
        assert_eq!(count, 1);
    }

    /// How long v14 holds the write lock on a large history. Prints rather
    /// than asserts. Run with
    /// `cargo test --release --bin suv v14_migration_cost -- --ignored --nocapture`.
    #[test]
    #[ignore = "builds a 100k-entry database; run explicitly with --ignored"]
    fn v14_migration_cost_on_a_large_history() {
        let temp_dir = TempDir::new().unwrap();
        let db_path = temp_dir.path().join("test.db");
        let mut conn = init_db(&db_path).unwrap();
        conn.execute("INSERT INTO sessions VALUES ('s1', 'host', 1000, NULL)", [])
            .unwrap();
        revert_to_v13(&conn);
        conn.execute_batch("BEGIN").unwrap();
        for i in 0..100_000_i64 {
            insert_entry(&conn, i + 1, &format!("cargo test case_{}", i % 1000));
        }
        conn.execute_batch("COMMIT").unwrap();
        let start = std::time::Instant::now();
        run_migrations(&mut conn).unwrap();
        println!("v14 at 100k entries: migration {:?}", start.elapsed());
    }

    /// Back to a v14 database: no `ai_snapshots` table.
    fn revert_to_v14(conn: &Connection) {
        conn.execute_batch(
            "DROP TABLE IF EXISTS ai_snapshots;
             UPDATE schema_version SET version = 14;",
        )
        .unwrap();
    }

    #[test]
    fn test_v15_adds_a_snapshot_table_holding_two_rows_per_session() {
        let temp_dir = TempDir::new().unwrap();
        let db_path = temp_dir.path().join("test.db");
        let conn = init_db(&db_path).unwrap();
        revert_to_v14(&conn);
        drop(conn);

        let conn = init_db(&db_path).unwrap();
        assert_eq!(get_schema_version(&conn).unwrap(), SCHEMA_VERSION);
        let insert = |kind: &str| {
            conn.execute(
                "INSERT INTO ai_snapshots (session_id, kind, at, cwd, repo_root, paths, path_count)
                 VALUES ('claude-s', ?1, 1, '/r', '/r', '[]', 0)",
                [kind],
            )
        };
        insert("start").unwrap();
        insert("end").unwrap();
        assert!(insert("end").is_err(), "one end snapshot per session");
        assert!(insert("turn").is_err(), "only start and end");
        // Another shell running it again changes nothing.
        migrate_v15(&conn).unwrap();
        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM ai_snapshots", [], |row| row.get(0))
            .unwrap();
        assert_eq!(count, 2);
    }

    /// How long v15 holds the write lock on a large history. Prints rather
    /// than asserts. Run with
    /// `cargo test --release --bin suv v15_migration_cost -- --ignored --nocapture`.
    #[test]
    #[ignore = "builds a 100k-entry database; run explicitly with --ignored"]
    fn v15_migration_cost_on_a_large_history() {
        let temp_dir = TempDir::new().unwrap();
        let db_path = temp_dir.path().join("test.db");
        let mut conn = init_db(&db_path).unwrap();
        conn.execute("INSERT INTO sessions VALUES ('s1', 'host', 1000, NULL)", [])
            .unwrap();
        revert_to_v14(&conn);
        conn.execute_batch("BEGIN").unwrap();
        for i in 0..100_000_i64 {
            insert_entry(&conn, i + 1, &format!("cargo test case_{}", i % 1000));
        }
        conn.execute_batch("COMMIT").unwrap();
        let start = std::time::Instant::now();
        run_migrations(&mut conn).unwrap();
        println!("v15 at 100k entries: migration {:?}", start.elapsed());
    }

    #[test]
    fn contains_ci_folds_case_rather_than_lowercasing() {
        let temp_dir = TempDir::new().unwrap();
        let conn = init_db(&temp_dir.path().join("test.db")).unwrap();
        let contains = |haystack: &str, needle: &str| -> bool {
            conn.query_row(
                "SELECT suvadu_contains_ci(?1, ?2)",
                params![haystack, needle],
                |row| row.get(0),
            )
            .unwrap()
        };
        for needle in ["σ", "Σ", "ς", "οδοσ", "ΟΔΟΣ", "οδος"] {
            assert!(contains("echo ΟΔΟΣ", needle), "{needle:?}");
            assert!(contains("echo οδος", needle), "{needle:?}");
        }
        for needle in ["straße", "STRAẞE", "STRASSE", "strasse"] {
            assert!(contains("cd Straße", needle), "{needle:?}");
            assert!(contains("cd STRASSE", needle), "{needle:?}");
        }
        assert!(contains("echo İstanbul", "İstanbul"));
        assert!(!contains("echo Istanbul", "İstanbul"));
        assert!(!contains("echo Istanbul", "ıstanbul"));
        assert!(!contains("echo ΟΔΟΣ", "οδοσσ"));
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
