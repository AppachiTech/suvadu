//! `suv redact`, end to end: stored text the rules would change is masked
//! wherever redaction writes, and no copy of it stays in any file `suv` owns,
//! apart from the pre-redact backup the run names.
//!
//! Secrets are written straight into the database, the way an older release
//! left them. Every test runs the real binary against a private HOME; nothing
//! here reads or writes the user's own database, backups or config.
#![cfg(unix)]

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

use rusqlite::{params, Connection};

/// A GitHub token, which the built-in rules recognise, unique to `tag`.
fn token(tag: &str) -> String {
    let body = format!("{tag}zqxJvkQzjXkqzVjqxKzqXjZvQkxzqJv12345");
    format!("ghp_{}", &body[..36])
}

const MASK: &str = "***REDACTED***";

struct Sandbox {
    home: tempfile::TempDir,
}

impl Sandbox {
    fn new() -> Self {
        Self {
            home: tempfile::tempdir().unwrap(),
        }
    }

    fn data_dir(&self) -> PathBuf {
        if cfg!(target_os = "macos") {
            self.home
                .path()
                .join("Library/Application Support/tech.appachi.suvadu")
        } else {
            self.home.path().join("data/suvadu")
        }
    }

    fn db_path(&self) -> PathBuf {
        self.data_dir().join("history.db")
    }

    fn backups_dir(&self) -> PathBuf {
        self.data_dir().join("backups")
    }

    fn write_config(&self, contents: &str) {
        let path = if cfg!(target_os = "macos") {
            self.data_dir().join("config.toml")
        } else {
            self.home.path().join("config/suvadu/config.toml")
        };
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, contents).unwrap();
    }

    /// The database, created with the current schema if it isn't there yet.
    fn database(&self) -> Connection {
        std::fs::create_dir_all(self.data_dir()).unwrap();
        suvadu::db::init_db(&self.db_path()).unwrap()
    }

    fn command(&self) -> Command {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_suv"));
        cmd.env("HOME", self.home.path())
            .env("XDG_DATA_HOME", self.home.path().join("data"))
            .env("XDG_CONFIG_HOME", self.home.path().join("config"))
            .env_remove("SUVADU_PAUSED")
            .env_remove("SUVADU_SESSION_ID")
            .env("NO_COLOR", "1")
            .stdin(Stdio::null());
        cmd
    }

    fn run(&self, args: &[&str]) -> Output {
        self.command().args(args).output().unwrap()
    }

    fn redact(&self, args: &[&str]) -> Output {
        let mut all = vec!["redact"];
        all.extend_from_slice(args);
        let out = self.run(&all);
        assert!(out.status.success(), "{}", all_output(&out));
        out
    }

    /// One row holding a secret in every place redaction writes to, each
    /// with its own token, and one row with nothing to mask.
    fn seed(&self) {
        let conn = self.database();
        seed_rows(&conn, "/project");
    }

    /// Every file under the data directory, except `except`, with its bytes.
    fn files(&self, except: Option<&Path>) -> Vec<(PathBuf, Vec<u8>)> {
        let mut found = Vec::new();
        let mut pending = vec![self.data_dir()];
        while let Some(dir) = pending.pop() {
            for entry in std::fs::read_dir(dir).unwrap().flatten() {
                let path = entry.path();
                if entry.file_type().unwrap().is_dir() {
                    pending.push(path);
                } else if Some(path.as_path()) != except {
                    found.push((path.clone(), std::fs::read(path).unwrap()));
                }
            }
        }
        found
    }

    fn assert_no_copy_of(&self, secret: &str, except: Option<&Path>) {
        for (path, bytes) in self.files(except) {
            let found = bytes
                .windows(secret.len())
                .any(|window| window == secret.as_bytes());
            assert!(!found, "{secret} is still in {}", path.display());
        }
    }

    /// The backups that hold a file name starting with `prefix`.
    fn backups_named(&self, prefix: &str) -> Vec<PathBuf> {
        let Ok(entries) = std::fs::read_dir(self.backups_dir()) else {
            return Vec::new();
        };
        entries
            .flatten()
            .map(|entry| entry.path())
            .filter(|path| {
                path.file_name()
                    .is_some_and(|name| name.to_string_lossy().starts_with(prefix))
            })
            .collect()
    }

    /// A project directory holding a `.suvadu.toml`.
    fn project_with_overlay(&self, overlay: &str) -> PathBuf {
        let dir = self.home.path().join("project");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join(".suvadu.toml"), overlay).unwrap();
        dir
    }
}

fn all_output(out: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    )
}

fn stored(conn: &Connection, sql: &str) -> String {
    conn.query_row(sql, [], |row| row.get(0)).unwrap()
}

fn seed_rows(conn: &Connection, cwd: &str) {
    let secret = |tag: &str| format!("password={}", token(tag));
    let context = serde_json::json!({
        "agent_prompt": secret("ctx"),
        "git_branch": format!("fix/{}", token("brn")),
        "agent_turn_id": "turn-1",
    })
    .to_string();
    conn.execute(
        "INSERT INTO sessions (id, hostname, created_at) VALUES ('s', 'h', 1)",
        [],
    )
    .unwrap();
    for (id, command, context) in [
        (1, format!("deploy {}", secret("cmd")), Some(context)),
        (2, "git status".to_string(), None),
    ] {
        conn.execute(
            "INSERT INTO entries (id, session_id, command, cwd, started_at, ended_at, \
             duration_ms, context) VALUES (?1, 's', ?2, ?3, 1, 2, 1, ?4)",
            params![id, command, cwd, context],
        )
        .unwrap();
    }
    conn.execute(
        "INSERT INTO entry_output (entry_id, stream, tail) VALUES (1, 'stderr', ?1)",
        [secret("out")],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO ai_sessions (id, native_id, agent, cwd, created_at, updated_at) \
         VALUES ('agent', 'n', 'claude-code', ?1, 1, 1)",
        [cwd],
    )
    .unwrap();
    let event = serde_json::json!({
        "id": "e1", "turn_id": "t1", "kind": "prompt", "at": 1, "model": null, "cwd": cwd,
        "data": {"text": secret("evt"), "truncated": false, "text_hash": null}
    })
    .to_string();
    conn.execute(
        "INSERT INTO ai_events (session_id, event_id, kind, cwd, data) \
         VALUES ('agent', 'e1', 'prompt', ?1, ?2)",
        params![cwd, event],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO ai_summaries (id, session_id, source_revision, text, agent, model, \
         source_ids, created_at) VALUES ('sum', 'agent', '0', ?1, 'a', 'm', '[]', 1)",
        [secret("sum")],
    )
    .unwrap();
    let paths = serde_json::json!([{"status": ".M", "path": format!("{}.txt", token("pth"))}]);
    conn.execute(
        "INSERT INTO ai_snapshots (session_id, kind, at, cwd, repo_root, branch, paths, \
         path_count) VALUES ('agent', 'start', 1, ?1, ?1, ?2, ?3, 1)",
        params![cwd, format!("fix/{}", token("snp")), paths.to_string()],
    )
    .unwrap();
}

/// Every token `seed_rows` wrote.
fn seeded_tokens() -> Vec<String> {
    ["cmd", "ctx", "brn", "out", "evt", "sum", "pth", "snp"]
        .into_iter()
        .map(token)
        .collect()
}

/// A backup as v0.4.0 would have left it: no agent tables worth the name, no
/// separator-insensitive index.
fn write_old_backup(path: &Path, secret: &str) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    let conn = Connection::open(path).unwrap();
    conn.execute_batch(
        "CREATE TABLE schema_version (version INTEGER NOT NULL);
         INSERT INTO schema_version VALUES (7);
         CREATE TABLE entries (id INTEGER PRIMARY KEY AUTOINCREMENT, command TEXT NOT NULL,
             cwd TEXT NOT NULL, context TEXT);
         CREATE VIRTUAL TABLE entries_fts USING fts5(command, content='entries',
             content_rowid='id', tokenize='trigram');
         CREATE TRIGGER entries_fts_ai AFTER INSERT ON entries BEGIN
             INSERT INTO entries_fts(rowid, command) VALUES (new.id, new.command);
         END;
         CREATE TRIGGER entries_fts_au AFTER UPDATE ON entries BEGIN
             INSERT INTO entries_fts(entries_fts, rowid, command)
                 VALUES ('delete', old.id, old.command);
             INSERT INTO entries_fts(rowid, command) VALUES (new.id, new.command);
         END;",
    )
    .unwrap();
    conn.execute(
        "INSERT INTO entries (command, cwd) VALUES (?1, '/old')",
        [format!("deploy password={secret}")],
    )
    .unwrap();
}

fn insert_commands(conn: &Connection, commands: &[String]) {
    conn.execute(
        "INSERT OR IGNORE INTO sessions (id, hostname, created_at) VALUES ('s', 'h', 1)",
        [],
    )
    .unwrap();
    for (id, command) in commands.iter().enumerate() {
        conn.execute(
            "INSERT INTO entries (id, session_id, command, cwd, started_at, ended_at, \
             duration_ms) VALUES (?1, 's', ?2, '/project', 1, 2, 1)",
            params![i64::try_from(id).unwrap() + 1, command],
        )
        .unwrap();
    }
}

/// A secret pasted into a prompt has no key name for a rule to find. Once a
/// rule has masked that value anywhere, the rest of the history is known to
/// hold it.
#[test]
fn a_value_masked_in_one_place_is_masked_wherever_else_it_appears() {
    let s = Sandbox::new();
    let value = "aB3dE6gH9jK2mN5pQ8sT1vW4yZ7cF0iL";
    let prose = format!("this is the JWT secret - {value} - in the backend service");
    let conn = s.database();
    insert_commands(
        &conn,
        &[
            format!("deploy JWT_SECRET={value}"),
            format!("echo {value} | pbcopy"),
        ],
    );
    let context = serde_json::json!({ "agent_prompt": prose }).to_string();
    conn.execute("UPDATE entries SET context = ?1 WHERE id = 2", [context])
        .unwrap();
    drop(conn);
    // In the older backup only as prose: the rule fired in the live database.
    let backup = s.backups_dir().join("history-1.db");
    write_old_backup(&backup, &token("old"));
    Connection::open(&backup)
        .unwrap()
        .execute("UPDATE entries SET command = ?1", [&prose])
        .unwrap();
    let cached = s.data_dir().join("prompts/session.prompt");
    std::fs::create_dir_all(cached.parent().unwrap()).unwrap();
    std::fs::write(&cached, &prose).unwrap();

    let out = s.redact(&["--dry-run"]);
    assert!(!all_output(&out).contains(value), "{}", all_output(&out));

    s.redact(&["--yes", "--no-backup"]);

    s.assert_no_copy_of(value, None);
    let conn = s.database();
    assert_eq!(
        stored(&conn, "SELECT command FROM entries WHERE id = 2"),
        format!("echo {MASK} | pbcopy")
    );
}

#[test]
fn a_variable_or_a_short_value_is_masked_where_it_was_found_and_nowhere_else() {
    let s = Sandbox::new();
    let conn = s.database();
    let elsewhere = [
        "echo ${DEPLOY_PASSWORD_VALUE}".to_string(),
        "echo $DEPLOY_PASSWORD_VALUE_2".to_string(),
        "grep hunter2 notes.txt".to_string(),
    ];
    let mut commands = vec![
        "login password=${DEPLOY_PASSWORD_VALUE}".to_string(),
        "login password=$DEPLOY_PASSWORD_VALUE_2".to_string(),
        "login password=hunter2".to_string(),
    ];
    commands.extend_from_slice(&elsewhere);
    insert_commands(&conn, &commands);
    drop(conn);

    s.redact(&["--yes", "--no-backup"]);

    let conn = s.database();
    for (id, command) in elsewhere.iter().enumerate() {
        let id = id + 4;
        assert_eq!(
            &stored(
                &conn,
                &format!("SELECT command FROM entries WHERE id = {id}")
            ),
            command
        );
    }
}

#[test]
fn a_dry_run_shows_the_masked_text_and_changes_nothing() {
    let s = Sandbox::new();
    s.seed();

    let out = s.redact(&["--dry-run"]);

    let text = all_output(&out);
    assert!(text.contains(MASK), "{text}");
    for label in [
        "Commands",
        "Command context",
        "Failure output",
        "Agent prompts and replies",
        "Agent summaries",
        "Git snapshots",
    ] {
        assert!(text.contains(label), "{label} missing from: {text}");
    }
    for secret in seeded_tokens() {
        assert!(!text.contains(&secret), "{secret} printed: {text}");
    }
    let conn = s.database();
    assert!(stored(&conn, "SELECT command FROM entries WHERE id = 1").contains(&token("cmd")));
    assert!(s.backups_named("preredact").is_empty());
}

#[test]
fn every_store_is_masked_and_a_row_with_nothing_to_mask_is_untouched() {
    let s = Sandbox::new();
    s.seed();

    s.redact(&["--yes"]);

    let conn = s.database();
    for sql in [
        "SELECT command FROM entries WHERE id = 1",
        "SELECT context FROM entries WHERE id = 1",
        "SELECT tail FROM entry_output",
        "SELECT data FROM ai_events",
        "SELECT text FROM ai_summaries",
        "SELECT branch FROM ai_snapshots",
        "SELECT paths FROM ai_snapshots",
    ] {
        let text = stored(&conn, sql);
        assert!(text.contains(MASK), "{sql}: {text}");
    }
    assert_eq!(
        stored(&conn, "SELECT command FROM entries WHERE id = 2"),
        "git status"
    );
}

#[test]
fn no_file_keeps_a_copy_of_the_secret() {
    let s = Sandbox::new();
    s.seed();
    s.write_config("");

    s.redact(&["--yes", "--no-backup"]);

    for secret in seeded_tokens() {
        s.assert_no_copy_of(&secret, None);
    }
}

#[test]
fn no_file_keeps_a_copy_while_another_process_has_the_database_open() {
    let s = Sandbox::new();
    s.seed();
    // An MCP server or an open recall screen. SQLite only empties the
    // write-ahead log by itself when the last connection closes, so with
    // this one open the log keeps whatever the run does not clear.
    let other = s.database();

    s.redact(&["--yes", "--no-backup"]);

    for secret in seeded_tokens() {
        s.assert_no_copy_of(&secret, None);
    }
    drop(other);
}

#[test]
fn freed_pages_are_gone_as_well_as_the_live_rows() {
    let s = Sandbox::new();
    s.seed();
    // Rows `suv delete` removed sit in freed pages until something reuses
    // them, and an in-place update leaves the same residue behind.
    let deleted = token("del");
    {
        let conn = s.database();
        conn.execute(
            "INSERT INTO entries (id, session_id, command, cwd, started_at, ended_at, \
             duration_ms) VALUES (3, 's', ?1, '/project', 1, 2, 1)",
            [format!("deploy password={deleted}")],
        )
        .unwrap();
        conn.execute("DELETE FROM entries WHERE id = 3", [])
            .unwrap();
    }

    s.redact(&["--yes", "--no-backup"]);

    s.assert_no_copy_of(&deleted, None);
}

#[test]
fn the_search_indexes_keep_nothing_of_the_old_text() {
    let s = Sandbox::new();
    s.seed();

    s.redact(&["--yes", "--no-backup"]);

    let conn = s.database();
    for index in ["entries_fts", "entries_norm_fts"] {
        // FTS5 only marks a replaced term deleted. An index that was rebuilt
        // is exactly the size of a fresh rebuild; one holding old terms in
        // tombstoned segments is larger.
        let size = |conn: &Connection| -> i64 {
            conn.query_row(
                &format!("SELECT sum(length(block)) FROM {index}_data"),
                [],
                |row| row.get(0),
            )
            .unwrap()
        };
        let after_the_run = size(&conn);
        conn.execute(
            &format!("INSERT INTO {index}({index}) VALUES('rebuild')"),
            [],
        )
        .unwrap();
        assert_eq!(after_the_run, size(&conn), "{index}");
    }
    let doctor = all_output(&s.run(&["doctor"]));
    assert!(!doctor.contains("no longer matches"), "{doctor}");
}

/// Leave `path` as a run that was stopped after committing would: rows
/// masked, the file not yet compacted, so stale copies are still in it.
fn stop_after_commit(repo: &suvadu::repository::Repository) {
    let mut mask = |text: &str, _: Option<&str>| suvadu::redact::redact_secrets(text);
    let plan = repo.find_unmasked(&mut mask).unwrap();
    assert!(repo.apply_redaction(&plan).unwrap() > 0);
}

/// Rows that make `SQLite` leave a copy behind when a page splits.
fn seed_long_values(conn: &Connection, value: &str) {
    conn.execute(
        "INSERT OR IGNORE INTO sessions (id, hostname, created_at) VALUES ('s', 'h', 1)",
        [],
    )
    .unwrap();
    for id in 100..130 {
        let command = if id % 2 == 0 {
            format!("password={value}")
        } else {
            "x".repeat(400)
        };
        conn.execute(
            "INSERT INTO entries (id, session_id, command, cwd, started_at, ended_at, \
             duration_ms) VALUES (?1, 's', ?2, '/project', 1, 2, 1)",
            params![id, command],
        )
        .unwrap();
    }
}

#[test]
fn a_run_stopped_before_compacting_is_finished_by_the_next_one() {
    let s = Sandbox::new();
    let value = token("lng").repeat(8);
    let start = &value[..40];
    {
        let conn = s.database();
        seed_long_values(&conn, &value);
        stop_after_commit(&suvadu::repository::Repository::new(conn));
    }
    let holds_it = s.files(None).iter().any(|(_, bytes)| {
        bytes
            .windows(start.len())
            .any(|window| window == start.as_bytes())
    });
    assert!(holds_it, "the fixture must leave the text behind");

    let out = s.redact(&["--yes"]);

    let text = all_output(&out);
    assert!(!text.contains("Nothing to mask"), "{text}");
    assert!(s.backups_named("preredact").is_empty(), "{text}");
    s.assert_no_copy_of(start, None);
}

#[test]
fn a_stopped_run_is_finished_in_a_backup_too() {
    let s = Sandbox::new();
    s.database();
    let value = token("lng").repeat(8);
    let start = &value[..40];
    let backup = s.backups_dir().join("history-20260909-203332.db");
    std::fs::create_dir_all(s.backups_dir()).unwrap();
    {
        let conn = suvadu::db::init_db(&backup).unwrap();
        seed_long_values(&conn, &value);
        stop_after_commit(&suvadu::repository::Repository::new(conn));
    }

    s.redact(&["--yes"]);

    s.assert_no_copy_of(start, None);
}

#[test]
fn a_dry_run_says_a_compaction_is_owed_and_does_not_do_it() {
    let s = Sandbox::new();
    let value = token("lng").repeat(8);
    {
        let conn = s.database();
        seed_long_values(&conn, &value);
        stop_after_commit(&suvadu::repository::Repository::new(conn));
    }

    let out = s.redact(&["--dry-run"]);

    let text = all_output(&out);
    assert!(text.contains("earlier run"), "{text}");
    let again = s.redact(&["--dry-run"]);
    assert!(all_output(&again).contains("earlier run"));
}

#[test]
fn a_second_run_finds_nothing_and_takes_no_backup() {
    let s = Sandbox::new();
    s.seed();
    s.redact(&["--yes", "--no-backup"]);

    let out = s.redact(&["--yes"]);

    let text = all_output(&out);
    assert!(text.contains("Nothing to mask"), "{text}");
    assert!(s.backups_named("preredact").is_empty(), "{text}");
}

#[test]
fn the_backup_taken_first_holds_the_original_and_the_run_says_so() {
    let s = Sandbox::new();
    s.seed();

    let out = s.redact(&["--yes"]);

    let backups = s.backups_named("preredact");
    assert_eq!(backups.len(), 1, "{backups:?}");
    let backup = &backups[0];

    let conn = Connection::open(backup).unwrap();
    let kept = stored(&conn, "SELECT command FROM entries WHERE id = 1");
    assert!(kept.contains(&token("cmd")), "{kept}");

    let text = all_output(&out);
    assert!(text.contains(&backup.display().to_string()), "{text}");
    assert!(text.contains("still holds"), "{text}");

    let mode = std::fs::metadata(backup).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode, 0o600);

    // Everything else is clean; that one file is the only exception.
    for secret in seeded_tokens() {
        s.assert_no_copy_of(&secret, Some(backup));
    }
}

#[test]
fn a_later_run_leaves_the_pre_redact_copy_alone_and_still_names_it() {
    let s = Sandbox::new();
    s.seed();
    s.redact(&["--yes"]);
    let copy = s.backups_named("preredact").remove(0);
    let before = std::fs::read(&copy).unwrap();

    // Run again to confirm it worked: the way back must survive that.
    let out = s.redact(&["--yes"]);

    let text = all_output(&out);
    assert!(text.contains("Nothing to mask"), "{text}");
    assert!(text.contains(&copy.display().to_string()), "{text}");
    assert!(text.contains("still holds"), "{text}");
    assert_eq!(std::fs::read(&copy).unwrap(), before);

    let dry = all_output(&s.redact(&["--dry-run"]));
    assert!(dry.contains(&copy.display().to_string()), "{dry}");
    assert!(dry.contains("history.db: 0 rows"), "{dry}");
    assert!(!dry.contains("Backups:"), "{dry}");
}

#[test]
fn no_backup_is_taken_with_no_backup() {
    let s = Sandbox::new();
    s.seed();

    s.redact(&["--yes", "--no-backup"]);

    assert!(s.backups_named("preredact").is_empty());
}

#[test]
fn nothing_changes_when_the_backup_cannot_be_written() {
    let s = Sandbox::new();
    s.seed();
    // A file where the backups directory should be.
    std::fs::write(s.backups_dir(), "").unwrap();

    let out = s.run(&["redact", "--yes"]);

    assert!(!out.status.success());
    let text = all_output(&out);
    assert!(text.contains("--no-backup"), "{text}");
    let conn = s.database();
    assert!(stored(&conn, "SELECT command FROM entries WHERE id = 1").contains(&token("cmd")));
}

#[test]
fn it_will_not_run_without_a_terminal_or_yes() {
    let s = Sandbox::new();
    s.seed();

    let out = s.run(&["redact"]);

    assert!(!out.status.success());
    let text = all_output(&out);
    assert!(text.contains("--yes"), "{text}");
    let conn = s.database();
    assert!(stored(&conn, "SELECT command FROM entries WHERE id = 1").contains(&token("cmd")));
}

#[test]
fn older_backups_are_masked_and_keep_the_schema_they_were_taken_under() {
    let s = Sandbox::new();
    s.seed();
    let secret = token("old");
    let old = s.backups_dir().join("history-20260909-203332.db");
    write_old_backup(&old, &secret);

    s.redact(&["--yes", "--no-backup"]);

    s.assert_no_copy_of(&secret, None);
    let conn = Connection::open(&old).unwrap();
    assert_eq!(
        stored(
            &conn,
            "SELECT CAST(max(version) AS TEXT) FROM schema_version"
        ),
        "7"
    );
    assert!(stored(&conn, "SELECT command FROM entries").contains(MASK));
    let hits: i64 = conn
        .query_row(
            "SELECT count(*) FROM entries_fts WHERE entries_fts MATCH ?1",
            [format!("\"{secret}\"")],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(hits, 0);
}

#[test]
fn an_older_backup_keeps_no_copy_in_its_freed_pages() {
    let s = Sandbox::new();
    s.seed();
    let old = s.backups_dir().join("history-20260909-203332.db");
    write_old_backup(&old, &token("old"));
    // A row deleted before the backup's rewrite leaves its text in the file,
    // where no row and no search index points at it.
    let deleted = token("gon");
    {
        let conn = Connection::open(&old).unwrap();
        conn.execute(
            "INSERT INTO entries (id, command, cwd) VALUES (2, ?1, '/old')",
            [format!("deploy password={deleted}")],
        )
        .unwrap();
        conn.execute_batch(
            "CREATE TABLE padding (body TEXT);
             INSERT INTO padding VALUES (printf('%.5000c', 'x'));",
        )
        .unwrap();
        conn.execute("UPDATE entries SET cwd = '/gone' WHERE id = 2", [])
            .unwrap();
        conn.execute_batch(
            "DROP TRIGGER entries_fts_au;
             DELETE FROM entries WHERE id = 2;
             INSERT INTO entries_fts(entries_fts) VALUES('rebuild');",
        )
        .unwrap();
    }
    let holds_it = |path: &Path| {
        std::fs::read(path)
            .unwrap()
            .windows(deleted.len())
            .any(|window| window == deleted.as_bytes())
    };
    assert!(holds_it(&old), "the fixture must leave the text behind");

    s.redact(&["--yes", "--no-backup"]);

    assert!(!holds_it(&old));
}

#[test]
fn a_dry_run_counts_older_backups_without_writing_to_them() {
    let s = Sandbox::new();
    s.seed();
    let old = s.backups_dir().join("history-20260909-203332.db");
    write_old_backup(&old, &token("old"));
    let before = std::fs::read(&old).unwrap();

    let out = s.redact(&["--dry-run"]);

    let text = all_output(&out);
    assert!(text.contains("history-20260909-203332.db"), "{text}");
    assert_eq!(std::fs::read(&old).unwrap(), before);
}

#[test]
fn a_backup_that_cannot_be_read_is_named_and_the_rest_still_finish() {
    let s = Sandbox::new();
    s.seed();
    let secret = token("old");
    write_old_backup(&s.backups_dir().join("history-1.db"), &secret);
    std::fs::write(s.backups_dir().join("broken.db"), "this is not a database").unwrap();

    let out = s.run(&["redact", "--yes", "--no-backup"]);

    assert!(!out.status.success(), "a skipped backup is not a clean run");
    let text = all_output(&out);
    assert!(text.contains("broken.db"), "{text}");
    s.assert_no_copy_of(&secret, None);
    s.assert_no_copy_of(&token("cmd"), None);
}

#[test]
fn prompt_cache_files_are_masked_in_place() {
    let s = Sandbox::new();
    s.seed();
    let secret = token("pmt");
    let nested = s.data_dir().join("prompts/claude/session");
    std::fs::create_dir_all(&nested).unwrap();
    let cached = nested.join("turn.prompt");
    std::fs::write(&cached, format!("deploy with password={secret}")).unwrap();
    let clean = s.data_dir().join("prompts/claude-clean.prompt");
    std::fs::write(&clean, "what does this do?").unwrap();

    s.redact(&["--yes", "--no-backup"]);

    s.assert_no_copy_of(&secret, None);
    assert!(std::fs::read_to_string(&cached).unwrap().contains(MASK));
    assert_eq!(
        std::fs::read_to_string(&clean).unwrap(),
        "what does this do?"
    );
}

#[test]
fn a_project_rule_reaches_the_rows_recorded_in_that_project() {
    let s = Sandbox::new();
    let project = s.project_with_overlay("[redaction]\nextra_patterns = [\"CORP-[0-9]+\"]\n");
    let conn = s.database();
    conn.execute(
        "INSERT INTO sessions (id, hostname, created_at) VALUES ('s', 'h', 1)",
        [],
    )
    .unwrap();
    for (id, cwd) in [(1, project.to_str().unwrap()), (2, "/elsewhere")] {
        conn.execute(
            "INSERT INTO entries (id, session_id, command, cwd, started_at, ended_at, \
             duration_ms) VALUES (?1, 's', 'ticket CORP-1234', ?2, 1, 2, 1)",
            params![id, cwd],
        )
        .unwrap();
    }
    drop(conn);

    s.redact(&["--yes", "--no-backup"]);

    let conn = s.database();
    assert_eq!(
        stored(&conn, "SELECT command FROM entries WHERE id = 1"),
        format!("ticket {MASK}")
    );
    assert_eq!(
        stored(&conn, "SELECT command FROM entries WHERE id = 2"),
        "ticket CORP-1234"
    );
}

#[test]
fn a_project_config_that_cannot_be_read_is_named_and_the_global_rules_still_run() {
    let s = Sandbox::new();
    let project = s.project_with_overlay("this is not = = toml");
    let secret = token("cmd");
    let conn = s.database();
    conn.execute(
        "INSERT INTO sessions (id, hostname, created_at) VALUES ('s', 'h', 1)",
        [],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO entries (id, session_id, command, cwd, started_at, ended_at, duration_ms) \
         VALUES (1, 's', ?1, ?2, 1, 2, 1)",
        params![
            format!("deploy password={secret}"),
            project.to_str().unwrap()
        ],
    )
    .unwrap();
    drop(conn);

    let out = s.redact(&["--yes", "--no-backup"]);

    let text = all_output(&out);
    assert!(text.contains(project.to_str().unwrap()), "{text}");
    s.assert_no_copy_of(&secret, None);
}

#[test]
fn it_runs_when_redaction_is_switched_off() {
    let s = Sandbox::new();
    s.write_config("[redaction]\nenabled = false\n");
    s.seed();

    s.redact(&["--yes", "--no-backup"]);

    s.assert_no_copy_of(&token("cmd"), None);
}

#[test]
fn bookmarks_notes_and_aliases_stay_as_written_and_the_run_says_so() {
    let s = Sandbox::new();
    s.seed();
    let secret = token("kpt");
    let command = format!("deploy password={secret}");
    {
        let conn = s.database();
        conn.execute(
            "INSERT INTO bookmarks (command, created_at) VALUES (?1, 1)",
            [&command],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO aliases (name, command, created_at) VALUES ('ship', ?1, 1)",
            [&command],
        )
        .unwrap();
    }

    let out = s.redact(&["--yes", "--no-backup"]);

    let text = all_output(&out);
    assert!(text.contains("1 bookmark"), "{text}");
    assert!(text.contains("1 alias"), "{text}");
    assert!(!text.contains(&secret), "{text}");
    let conn = s.database();
    assert_eq!(stored(&conn, "SELECT command FROM bookmarks"), command);
    assert_eq!(stored(&conn, "SELECT command FROM aliases"), command);
}

#[test]
fn nothing_to_mask_leaves_the_database_alone() {
    let s = Sandbox::new();
    {
        let conn = s.database();
        conn.execute(
            "INSERT INTO sessions (id, hostname, created_at) VALUES ('s', 'h', 1)",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO entries (session_id, command, cwd, started_at, ended_at, duration_ms) \
             VALUES ('s', 'git status', '/project', 1, 2, 1)",
            [],
        )
        .unwrap();
    }

    let out = s.redact(&["--yes"]);

    let text = all_output(&out);
    assert!(text.contains("Nothing to mask"), "{text}");
    assert!(s.backups_named("preredact").is_empty(), "{text}");
}
