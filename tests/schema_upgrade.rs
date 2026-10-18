//! Upgrade fixtures built from the schemas Suvadu actually released, plus the
//! backup/restore round trip a user performs after an upgrade goes wrong.
//!
//! The fixtures are literal DDL snapshots, not `init_db` output: a migration
//! chain cannot be used to prove itself, and nothing here assumes migrations
//! are reversible. Released schema versions (from the tagged `SCHEMA_VERSION`
//! constant):
//!
//! | database | shipped in |
//! |---|---|
//! | unversioned (no `schema_version` table) | pre-release / pre-versioning installs |
//! | v5 | v0.1.3 – v0.3.7 |
//! | v7 | v0.4.0 |
//! | v9 | v0.4.1 – v0.5.0 (current) |
//!
//! Every test writes to a `tempfile` directory. Nothing reads or writes the
//! user's real database, backups or config.

use std::path::Path;

use rusqlite::Connection;
use suvadu::ai_sessions::CapturePolicy;
use suvadu::db;
use suvadu::repository::Repository;

/// The schema version an upgrade must end at.
const CURRENT_SCHEMA: i64 = 11;

/// `entries`/`sessions`/`tags`/`notes`/`bookmarks` exactly as migration v1
/// created them: the three late columns on `entries` arrive by `ALTER TABLE`,
/// so a real released database has them last, in this order.
const V1_CORE: &str = r"
CREATE TABLE tags (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    name TEXT UNIQUE NOT NULL,
    description TEXT
);
CREATE TABLE sessions (
    id TEXT PRIMARY KEY,
    hostname TEXT NOT NULL,
    created_at INTEGER NOT NULL,
    tag_id INTEGER REFERENCES tags(id)
);
CREATE TABLE entries (
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
);
ALTER TABLE entries ADD COLUMN tag_id INTEGER REFERENCES tags(id);
ALTER TABLE entries ADD COLUMN executor_type TEXT;
ALTER TABLE entries ADD COLUMN executor TEXT;
CREATE INDEX idx_entries_session_id ON entries(session_id);
CREATE INDEX idx_entries_started_at ON entries(started_at);
CREATE INDEX idx_entries_command    ON entries(command);
CREATE INDEX idx_entries_tag_id     ON entries(tag_id);
CREATE TABLE bookmarks (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    command TEXT NOT NULL UNIQUE,
    label TEXT,
    created_at INTEGER NOT NULL
);
CREATE TABLE notes (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    entry_id INTEGER NOT NULL UNIQUE REFERENCES entries(id) ON DELETE CASCADE,
    note TEXT NOT NULL,
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL
);
";

/// Migrations v2–v5: index-only, plus the `aliases` table from v3.
const V2_TO_V5: &str = r"
CREATE INDEX idx_entries_exit_code_started ON entries(exit_code, started_at);
CREATE INDEX idx_entries_cwd_started       ON entries(cwd, started_at);
CREATE INDEX idx_entries_executor_type     ON entries(executor_type);
CREATE TABLE aliases (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    name TEXT NOT NULL UNIQUE,
    command TEXT NOT NULL,
    created_at INTEGER NOT NULL
);
CREATE INDEX idx_entries_command_started ON entries(command, started_at);
CREATE INDEX idx_entries_started_command ON entries(started_at, command);
CREATE INDEX idx_entries_started_cwd     ON entries(started_at, cwd);
";

/// Migrations v6 (skills) and v7 (FTS5 trigram index over `entries.command`).
const V6_TO_V7: &str = r"
CREATE TABLE skills (
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
);
CREATE UNIQUE INDEX idx_skills_name_scope ON skills(name, scope);
CREATE INDEX idx_skills_status ON skills(status);
CREATE VIRTUAL TABLE entries_fts USING fts5(
    command,
    content='entries',
    content_rowid='id',
    tokenize='trigram'
);
CREATE TRIGGER entries_fts_ai AFTER INSERT ON entries BEGIN
    INSERT INTO entries_fts(rowid, command) VALUES (new.id, new.command);
END;
CREATE TRIGGER entries_fts_ad AFTER DELETE ON entries BEGIN
    INSERT INTO entries_fts(entries_fts, rowid, command) VALUES ('delete', old.id, old.command);
END;
CREATE TRIGGER entries_fts_au AFTER UPDATE ON entries BEGIN
    INSERT INTO entries_fts(entries_fts, rowid, command) VALUES ('delete', old.id, old.command);
    INSERT INTO entries_fts(rowid, command) VALUES (new.id, new.command);
END;
";

/// History a released install would already hold: a tagged session, an agent
/// command, a non-ASCII command, a note, a bookmark and an alias.
const SEED_DATA: &str = r"
INSERT INTO tags (id, name, description) VALUES (1, 'work', 'work project');
INSERT INTO sessions (id, hostname, created_at, tag_id) VALUES ('shell-1', 'oldhost', 1000, 1);
INSERT INTO sessions (id, hostname, created_at, tag_id) VALUES ('shell-2', 'oldhost', 2000, NULL);
INSERT INTO entries (id, session_id, command, cwd, exit_code, started_at, ended_at, duration_ms, context, tag_id, executor_type, executor)
    VALUES (1, 'shell-1', 'git commit -m ''ancient history''', '/work/project', 0, 1000, 1100, 100, NULL, 1, NULL, NULL);
INSERT INTO entries (id, session_id, command, cwd, exit_code, started_at, ended_at, duration_ms, context, tag_id, executor_type, executor)
    VALUES (2, 'shell-1', 'cargo build --release', '/work/project', 1, 2000, 2500, 500, NULL, NULL, 'agent', 'claude-code');
INSERT INTO entries (id, session_id, command, cwd, exit_code, started_at, ended_at, duration_ms, context, tag_id, executor_type, executor)
    VALUES (3, 'shell-2', 'grep Émile notes.txt', '/work/project', 0, 3000, 3100, 100, NULL, NULL, NULL, NULL);
INSERT INTO notes (entry_id, note, created_at, updated_at) VALUES (1, 'the commit that started it', 1000, 1000);
INSERT INTO bookmarks (command, label, created_at) VALUES ('cargo build --release', 'build', 1000);
INSERT INTO aliases (name, command, created_at) VALUES ('gs', 'git status', 1000);
";

fn set_version(conn: &Connection, version: i64) {
    conn.execute_batch(&format!(
        "CREATE TABLE schema_version (version INTEGER NOT NULL);
         INSERT INTO schema_version VALUES ({version});"
    ))
    .unwrap();
}

/// Write a database file exactly as the named released version left it.
fn released_database(dir: &Path, version: i64) -> std::path::PathBuf {
    let path = dir.join(format!("history-v{version}.db"));
    let conn = Connection::open(&path).unwrap();
    conn.execute_batch(V1_CORE).unwrap();
    conn.execute_batch(V2_TO_V5).unwrap();
    if version >= 6 {
        conn.execute_batch(V6_TO_V7).unwrap();
    }
    // v0.4.0 kept the index in sync from the moment it was created: its
    // triggers, created above, index the seed rows as they are written.
    // Indexing them again here would hand the upgrade a corrupt index.
    conn.execute_batch(SEED_DATA).unwrap();
    set_version(&conn, version);
    drop(conn);
    path
}

fn scalar_i64(conn: &Connection, sql: &str) -> i64 {
    conn.query_row(sql, [], |row| row.get(0)).unwrap()
}

fn table_exists(conn: &Connection, table: &str) -> bool {
    scalar_i64(
        conn,
        &format!("SELECT COUNT(*) FROM sqlite_master WHERE name='{table}'"),
    ) > 0
}

fn schema_version(conn: &Connection) -> i64 {
    scalar_i64(conn, "SELECT version FROM schema_version LIMIT 1")
}

/// Every record a released database held must survive the upgrade unchanged.
fn assert_seed_data_intact(conn: &Connection) {
    assert_eq!(scalar_i64(conn, "SELECT COUNT(*) FROM entries"), 3);
    assert_eq!(scalar_i64(conn, "SELECT COUNT(*) FROM sessions"), 2);
    assert_eq!(scalar_i64(conn, "SELECT COUNT(*) FROM notes"), 1);
    assert_eq!(scalar_i64(conn, "SELECT COUNT(*) FROM bookmarks"), 1);
    assert_eq!(scalar_i64(conn, "SELECT COUNT(*) FROM aliases"), 1);
    assert_eq!(scalar_i64(conn, "SELECT COUNT(*) FROM tags"), 1);
    let oldest: String = conn
        .query_row("SELECT command FROM entries WHERE id=1", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(oldest, "git commit -m 'ancient history'");
    let note: String = conn
        .query_row("SELECT note FROM notes WHERE entry_id=1", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(note, "the commit that started it");
}

/// The FTS index must cover every row, exactly once, and pass FTS5's own
/// integrity check — a backfill that double-inserted would still "find" a
/// command while quietly corrupting the index.
fn assert_fts_covers_all_entries(conn: &Connection) {
    let entries = scalar_i64(conn, "SELECT COUNT(*) FROM entries");
    let indexed = scalar_i64(conn, "SELECT COUNT(*) FROM entries_fts");
    assert_eq!(indexed, entries, "every entry must be indexed exactly once");
    // `rank = 1` makes FTS5 also compare the index against `entries`, which
    // is the only way it notices a row indexed twice.
    conn.execute_batch("INSERT INTO entries_fts(entries_fts, rank) VALUES('integrity-check', 1);")
        .expect("FTS index must match the entries it indexes after upgrade");
    let hits = scalar_i64(
        conn,
        "SELECT COUNT(*) FROM entries_fts WHERE command LIKE '%ancient history%'",
    );
    assert_eq!(hits, 1, "a pre-upgrade command must be searchable");

    // The separator-insensitive index (v11) covers the same history.
    conn.execute_batch(
        "INSERT INTO entries_norm_fts(entries_norm_fts, rank) VALUES('integrity-check', 1);",
    )
    .expect("separator index must match the entries it indexes after upgrade");
    let loose_hits = scalar_i64(
        conn,
        "SELECT COUNT(*) FROM entries e WHERE e.id IN \
         (SELECT rowid FROM entries_norm_fts WHERE command_norm LIKE '%ancienthistory%')",
    );
    assert_eq!(
        loose_hits, 1,
        "a pre-upgrade command must be searchable without separators"
    );
}

/// Tables and columns the current code assumes exist.
fn assert_current_schema_present(conn: &Connection) {
    for table in [
        "skills",
        "entries_fts",
        "entries_norm_fts",
        "ai_sessions",
        "ai_events",
        "ai_sources",
        "ai_summaries",
    ] {
        assert!(
            table_exists(conn, table),
            "missing table after upgrade: {table}"
        );
    }
    for column in [
        "source_event_count",
        "source_command_count",
        "source_prefix_hash",
        "base_summary_id",
    ] {
        assert_eq!(
            scalar_i64(
                conn,
                &format!(
                    "SELECT COUNT(*) FROM pragma_table_info('ai_summaries') WHERE name='{column}'"
                )
            ),
            1,
            "missing ai_summaries column after upgrade: {column}"
        );
    }
    // Added to ai_sources after the first checkpointed release; a database
    // created by an older build must still end up with them.
    for column in ["skipped", "gaps"] {
        assert_eq!(
            scalar_i64(
                conn,
                &format!(
                    "SELECT COUNT(*) FROM pragma_table_info('ai_sources') WHERE name='{column}'"
                )
            ),
            1,
            "missing ai_sources column after upgrade: {column}"
        );
    }
}

#[test]
fn v5_database_from_v0_3_7_upgrades_with_every_record_intact() {
    let dir = tempfile::tempdir().unwrap();
    let path = released_database(dir.path(), 5);

    // The fixture really is a v5 database: nothing the upgrade adds is
    // present yet, so these assertions would fail if migrations never ran.
    {
        let conn = Connection::open(&path).unwrap();
        assert_eq!(schema_version(&conn), 5);
        assert!(!table_exists(&conn, "skills"));
        assert!(!table_exists(&conn, "entries_fts"));
        assert!(!table_exists(&conn, "ai_sessions"));
    }

    let conn = db::init_db(&path).unwrap();
    assert_eq!(schema_version(&conn), CURRENT_SCHEMA);
    assert_seed_data_intact(&conn);
    assert_current_schema_present(&conn);
    assert_fts_covers_all_entries(&conn);
    let integrity: String = conn
        .query_row("PRAGMA integrity_check", [], |row| row.get(0))
        .unwrap();
    assert_eq!(integrity, "ok");
}

#[test]
fn v7_database_from_v0_4_0_upgrades_without_reindexing_twice() {
    let dir = tempfile::tempdir().unwrap();
    let path = released_database(dir.path(), 7);

    {
        let conn = Connection::open(&path).unwrap();
        assert_eq!(schema_version(&conn), 7);
        assert!(table_exists(&conn, "entries_fts"));
        assert!(!table_exists(&conn, "ai_summaries"));
    }

    let conn = db::init_db(&path).unwrap();
    assert_eq!(schema_version(&conn), CURRENT_SCHEMA);
    assert_seed_data_intact(&conn);
    assert_current_schema_present(&conn);
    // v7 already had the index: the upgrade must not backfill it a second time.
    assert_fts_covers_all_entries(&conn);
}

#[test]
fn upgraded_history_is_readable_and_searchable_through_the_repository() {
    let dir = tempfile::tempdir().unwrap();
    let path = released_database(dir.path(), 5);
    let repo = Repository::new(db::init_db(&path).unwrap());

    let all = repo
        .get_entries_filtered(100, 0, &suvadu::repository::QueryFilter::default())
        .unwrap();
    assert_eq!(all.len(), 3);

    // A command recorded by the oldest supported release is still a hit.
    let hits = repo
        .get_entries_filtered(
            100,
            0,
            &suvadu::repository::QueryFilter {
                query: Some("ancient history"),
                ..Default::default()
            },
        )
        .unwrap();
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].tag_name.as_deref(), Some("work"));
}

/// Installs from before schema versioning existed have no `schema_version`
/// table at all — only the original tables and their data.
#[test]
fn an_unversioned_pre_release_database_upgrades_and_keeps_its_history() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("legacy.db");
    let conn = Connection::open(&path).unwrap();
    conn.execute_batch(V1_CORE).unwrap();
    // v3's aliases table exists in the fixture data, so seed without it.
    conn.execute_batch(
        "INSERT INTO tags (id, name, description) VALUES (1, 'work', 'work project');
         INSERT INTO sessions (id, hostname, created_at, tag_id) VALUES ('shell-1', 'oldhost', 1000, 1);
         INSERT INTO entries (id, session_id, command, cwd, exit_code, started_at, ended_at, duration_ms, context, tag_id, executor_type, executor)
             VALUES (1, 'shell-1', 'git commit -m ''ancient history''', '/work', 0, 1000, 1100, 100, NULL, 1, NULL, NULL);",
    )
    .unwrap();
    assert!(!table_exists(&conn, "schema_version"));
    assert!(!table_exists(&conn, "aliases"));
    drop(conn);

    let conn = db::init_db(&path).unwrap();
    assert_eq!(schema_version(&conn), CURRENT_SCHEMA);
    assert_eq!(scalar_i64(&conn, "SELECT COUNT(*) FROM entries"), 1);
    assert!(table_exists(&conn, "aliases"));
    assert_current_schema_present(&conn);
    assert_fts_covers_all_entries(&conn);
}

/// Restoring an old backup is an upgrade: the file was written by whichever
/// release took it, not by the one reading it now.
#[test]
fn a_backup_written_by_an_older_release_upgrades_when_restored() {
    let dir = tempfile::tempdir().unwrap();
    let old_backup = released_database(dir.path(), 5);
    let restored = dir.path().join("history.db");
    std::fs::copy(&old_backup, &restored).unwrap();

    let repo = Repository::new(db::init_db(&restored).unwrap());
    let entries = repo
        .get_entries_filtered(100, 0, &suvadu::repository::QueryFilter::default())
        .unwrap();
    assert_eq!(entries.len(), 3);
    // The restored copy is usable immediately, including for agent records
    // whose tables the old backup never had.
    assert_eq!(
        repo.list_ai_sessions(10, 0, &[]).unwrap()["sessions"]
            .as_array()
            .unwrap()
            .len(),
        0
    );
}

#[test]
fn upgrading_twice_changes_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let path = released_database(dir.path(), 5);

    let conn = db::init_db(&path).unwrap();
    let first = scalar_i64(&conn, "SELECT COUNT(*) FROM entries_fts");
    drop(conn);

    let conn = db::init_db(&path).unwrap();
    assert_eq!(schema_version(&conn), CURRENT_SCHEMA);
    assert_eq!(scalar_i64(&conn, "SELECT COUNT(*) FROM entries_fts"), first);
    assert_seed_data_intact(&conn);
}

/// v0.4.0's update trigger re-indexed a row on any change to it; after the
/// upgrade only a change to the command does.
#[test]
fn an_upgraded_index_follows_command_edits_and_ignores_context_edits() {
    let dir = tempfile::tempdir().unwrap();
    let path = released_database(dir.path(), 7);
    let conn = db::init_db(&path).unwrap();

    let trigger: String = conn
        .query_row(
            "SELECT sql FROM sqlite_master WHERE type='trigger' AND name='entries_fts_au'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert!(
        trigger.contains("AFTER UPDATE OF id, command ON entries"),
        "{trigger}"
    );

    let blocks = |conn: &Connection| -> Vec<Vec<u8>> {
        conn.prepare("SELECT block FROM entries_fts_data ORDER BY id")
            .unwrap()
            .query_map([], |row| row.get(0))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap()
    };
    let before = blocks(&conn);
    conn.execute(
        "UPDATE entries SET context = '{\"agent\":\"codex\"}' WHERE id = 2",
        [],
    )
    .unwrap();
    assert_eq!(blocks(&conn), before, "a context edit re-indexed the row");

    conn.execute(
        "UPDATE entries SET command = 'git commit -m ''recent history''' WHERE id = 1",
        [],
    )
    .unwrap();
    assert_eq!(
        scalar_i64(
            &conn,
            "SELECT COUNT(*) FROM entries_fts WHERE command LIKE '%recent history%'"
        ),
        1
    );
    assert_eq!(
        scalar_i64(
            &conn,
            "SELECT COUNT(*) FROM entries_fts WHERE command LIKE '%ancient history%'"
        ),
        0
    );
    conn.execute_batch("INSERT INTO entries_fts(entries_fts, rank) VALUES('integrity-check', 1);")
        .unwrap();
}

/// Several shells starting at once after an upgrade all open the old
/// database together. Exactly one may migrate it; the rest must wait for it
/// (within the busy timeout) rather than fail, and must not re-run a step
/// it already applied.
#[test]
fn shells_opening_an_old_database_at_once_all_upgrade_it_safely() {
    // How the openers interleave is up to the scheduler, so race them a few
    // times; any one bad interleaving is a failure.
    for _ in 0..4 {
        race_shells_to_upgrade();
    }
}

fn race_shells_to_upgrade() {
    const SHELLS: usize = 12;
    const EXTRA_ROWS: i64 = 5_000;

    let dir = tempfile::tempdir().unwrap();
    let path = released_database(dir.path(), 5);
    {
        // Released databases are already in WAL mode, and enough history
        // makes the FTS backfill slow enough for the openers to overlap.
        let conn = Connection::open(&path).unwrap();
        conn.pragma_update(None, "journal_mode", "WAL").unwrap();
        conn.execute_batch("BEGIN").unwrap();
        let mut insert = conn
            .prepare(
                "INSERT INTO entries (session_id, command, cwd, exit_code, started_at, ended_at, duration_ms)
                 VALUES ('shell-2', ?1, '/work', 0, ?2, ?2, 1)",
            )
            .unwrap();
        for i in 0..EXTRA_ROWS {
            insert
                .execute(rusqlite::params![
                    format!("echo history line {i}"),
                    10_000 + i
                ])
                .unwrap();
        }
        drop(insert);
        conn.execute_batch("COMMIT").unwrap();
    }

    let barrier = std::sync::Arc::new(std::sync::Barrier::new(SHELLS));
    // Every shell must be spawned before any is joined, or the barrier never
    // releases.
    #[allow(clippy::needless_collect)]
    let handles: Vec<_> = (0..SHELLS)
        .map(|shell| {
            let path = path.clone();
            let barrier = std::sync::Arc::clone(&barrier);
            std::thread::spawn(move || -> Result<(), String> {
                barrier.wait();
                let conn = db::init_db(&path).map_err(|e| format!("open: {e}"))?;
                // What `suv add` does next: record the command it was run for.
                let repo = Repository::new(conn);
                let stamp = 1_000_000 + i64::try_from(shell).unwrap();
                repo.insert_entry(&suvadu::models::Entry::new(
                    "shell-2".to_string(),
                    format!("echo recorded by shell {shell}"),
                    "/work".to_string(),
                    Some(0),
                    stamp,
                    stamp,
                ))
                .map(|_| ())
                .map_err(|e| format!("record: {e}"))
            })
        })
        .collect();
    let outcomes: Vec<Result<(), String>> =
        handles.into_iter().map(|h| h.join().unwrap()).collect();
    assert!(
        outcomes.iter().all(Result::is_ok),
        "every shell must open the database and record its command: {outcomes:?}"
    );

    let conn = Connection::open(&path).unwrap();
    assert_eq!(schema_version(&conn), CURRENT_SCHEMA);
    let shells = i64::try_from(SHELLS).unwrap();
    assert_eq!(
        scalar_i64(&conn, "SELECT COUNT(*) FROM entries"),
        3 + EXTRA_ROWS + shells
    );
    assert_fts_covers_all_entries(&conn);
    assert_eq!(
        scalar_i64(
            &conn,
            "SELECT COUNT(*) FROM entries_fts WHERE command LIKE '%recorded by shell%'"
        ),
        shells
    );
    let integrity: String = conn
        .query_row("PRAGMA integrity_check", [], |row| row.get(0))
        .unwrap();
    assert_eq!(integrity, "ok");
}

#[test]
fn a_database_from_a_newer_suvadu_is_refused_rather_than_downgraded() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("future.db");
    let conn = Connection::open(&path).unwrap();
    conn.execute_batch(V1_CORE).unwrap();
    conn.execute_batch(V2_TO_V5).unwrap();
    conn.execute_batch(V6_TO_V7).unwrap();
    conn.execute_batch(SEED_DATA).unwrap();
    set_version(&conn, 99);
    drop(conn);

    let err = db::init_db(&path).unwrap_err().to_string();
    assert!(err.contains("newer"), "unexpected error: {err}");

    // The refusal must leave the data untouched.
    let conn = Connection::open(&path).unwrap();
    assert_eq!(schema_version(&conn), 99);
    assert_eq!(scalar_i64(&conn, "SELECT COUNT(*) FROM entries"), 3);
}

// ── Backup and restore ──────────────────────────────────────────────

/// A minimal Codex transcript: one prompt, one answer, in one session.
fn codex_transcript(dir: &Path) -> std::path::PathBuf {
    let path = dir.join("codex-session.jsonl");
    let lines = [
        r#"{"timestamp":"2026-09-12T12:00:00Z","type":"session_meta","payload":{"id":"native-7","cwd":"/work/project"}}"#,
        r#"{"timestamp":"2026-09-12T12:00:01Z","type":"turn_context","payload":{"turn_id":"turn-1","cwd":"/work/project","model":"test-model"}}"#,
        r#"{"timestamp":"2026-09-12T12:00:02Z","type":"event_msg","payload":{"type":"user_message","message":"Restore the database"}}"#,
        r#"{"timestamp":"2026-09-12T12:00:03Z","type":"event_msg","payload":{"type":"agent_message","phase":"final_answer","message":"Restored from backup."}}"#,
    ];
    std::fs::write(&path, format!("{}\n", lines.join("\n"))).unwrap();
    path
}

/// Seed one upgraded database with shell history, an agent session and a
/// summary checkpoint, and return its path.
fn seeded_live_database(dir: &Path) -> std::path::PathBuf {
    let path = released_database(dir, 5);
    let repo = Repository::new(db::init_db(&path).unwrap());
    repo.import_codex_session(&codex_transcript(dir), None, |_| {
        Ok(CapturePolicy::default())
    })
    .unwrap();

    let session = repo.get_ai_session("codex-native-7", 50, 0, &[]).unwrap();
    let summary = suvadu::ai_sessions::SummaryInput {
        session_id: "codex-native-7".to_string(),
        source_revision: session["session"]["revision"].as_str().unwrap().to_string(),
        text: "Checked the restore path end to end.".to_string(),
        agent: "codex".to_string(),
        model: "test-model".to_string(),
        source_ids: vec![session["events"][0]["id"].as_str().unwrap().to_string()],
        base_summary_id: None,
    };
    repo.save_ai_summary(&summary, &[]).unwrap();
    path
}

#[test]
fn a_backup_restores_history_and_agent_session_records() {
    let dir = tempfile::tempdir().unwrap();
    let live = seeded_live_database(dir.path());
    let backup = dir.path().join("backups").join("history-fixture.db");
    std::fs::create_dir_all(backup.parent().unwrap()).unwrap();
    Repository::new(db::init_db(&live).unwrap())
        .backup_to(&backup)
        .unwrap();

    // Restore the way a user does: put the backup file where the database
    // lives and open it.
    let restored = dir.path().join("restored.db");
    std::fs::copy(&backup, &restored).unwrap();
    let repo = Repository::new(db::init_db(&restored).unwrap());

    // Shell history.
    let entries = repo
        .get_entries_filtered(100, 0, &suvadu::repository::QueryFilter::default())
        .unwrap();
    assert_eq!(entries.len(), 3);

    // Agent session records: the session, its events and its summary.
    let listed = repo.list_ai_sessions(10, 0, &[]).unwrap();
    assert_eq!(listed["sessions"].as_array().unwrap().len(), 1);
    let session = repo.get_ai_session("codex-native-7", 50, 0, &[]).unwrap();
    assert_eq!(session["session"]["agent"], "openai-codex");
    assert!(
        session["events"]
            .as_array()
            .is_some_and(|events| !events.is_empty()),
        "restored session must keep its captured events"
    );
    let summaries = repo.ai_summaries_for_session("codex-native-7").unwrap();
    assert_eq!(summaries.len(), 1);
    assert_eq!(summaries[0].text, "Checked the restore path end to end.");

    // And the restored copy is still a healthy, writable database.
    let integrity: String = repo_integrity(&restored);
    assert_eq!(integrity, "ok");
}

fn repo_integrity(path: &Path) -> String {
    let conn = Connection::open(path).unwrap();
    conn.query_row("PRAGMA integrity_check", [], |row| row.get(0))
        .unwrap()
}

#[test]
fn a_backup_taken_before_a_delete_still_contains_the_deleted_history() {
    let dir = tempfile::tempdir().unwrap();
    let live = seeded_live_database(dir.path());
    let repo = Repository::new(db::init_db(&live).unwrap());

    let backup = dir.path().join("predelete.db");
    repo.backup_to(&backup).unwrap();
    let deleted = repo.delete_entries("ancient history", false, None).unwrap();
    assert_eq!(deleted, 1);

    // Gone from the live database...
    assert_eq!(
        repo.count_entries_by_pattern("ancient history", false, None)
            .unwrap(),
        0
    );
    // ...and still readable in the backup. This is what the deletion notice
    // has to disclose.
    let backup_repo = Repository::new(db::init_db(&backup).unwrap());
    assert_eq!(
        backup_repo
            .count_entries_by_pattern("ancient history", false, None)
            .unwrap(),
        1
    );
}
