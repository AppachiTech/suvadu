//! End-to-end tests for `suv import --from atuin-db`.
//!
//! Every test builds its own Atuin-shaped database in a temporary directory and
//! runs the real binary against a private HOME. The user's own Atuin database
//! and the user's own Suvadu database are never opened.
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use rusqlite::{params, Connection};

/// The `_sqlx_migrations` rows an Atuin 18.22.0 `history.db` carries.
const MIGRATIONS_18_22: &[i64] = &[
    20_210_422_143_411,
    20_220_505_083_406,
    20_220_806_155_627,
    20_230_315_220_114,
    20_230_319_185_725,
    20_260_224_000_100,
    20_260_709_214_605,
    20_260_723_000_000,
    20_260_723_000_001,
    20_260_723_000_002,
    20_260_723_000_003,
    20_260_818_000_000,
];

/// One row of Atuin's `history` table.
struct Row {
    id: String,
    timestamp: i64,
    duration: i64,
    exit: i64,
    command: String,
    cwd: String,
    session: String,
    hostname: String,
    deleted_at: Option<i64>,
    author: Option<String>,
    intent: Option<String>,
    shell: Option<String>,
    author_kind: Option<i64>,
}

impl Row {
    fn new(id: &str, timestamp_ns: i64, command: &str) -> Self {
        Self {
            id: id.to_string(),
            timestamp: timestamp_ns,
            duration: 1_500_000_000,
            exit: 0,
            command: command.to_string(),
            cwd: "/home/ellie/work".to_string(),
            session: "0193c0ffee".to_string(),
            hostname: "laptop:ellie".to_string(),
            deleted_at: None,
            author: None,
            intent: None,
            shell: Some("bash".to_string()),
            author_kind: Some(1),
        }
    }
}

/// Build an Atuin 18.22.0-shaped `history.db` with the given rows.
fn atuin_db(path: &Path, rows: &[Row]) {
    atuin_db_with_migrations(path, MIGRATIONS_18_22, rows);
}

fn atuin_db_with_migrations(path: &Path, migrations: &[i64], rows: &[Row]) {
    let conn = Connection::open(path).unwrap();
    conn.execute_batch(
        "create table if not exists history (
            id text primary key,
            timestamp integer not null,
            duration integer not null,
            exit integer not null,
            command text not null,
            cwd text not null,
            session text not null,
            hostname text not null,
            deleted_at integer,
            author text,
            intent text,
            shell text,
            author_kind integer
        );
        create table if not exists _sqlx_migrations (
            version bigint primary key,
            description text not null,
            installed_on timestamp not null default current_timestamp,
            success boolean not null,
            checksum blob not null,
            execution_time bigint not null
        );",
    )
    .unwrap();
    for v in migrations {
        conn.execute(
            "insert into _sqlx_migrations (version, description, success, checksum, execution_time)
             values (?1, 'test', 1, x'00', 0)",
            params![v],
        )
        .unwrap();
    }
    for r in rows {
        conn.execute(
            "insert into history (id, timestamp, duration, exit, command, cwd, session, hostname,
                                  deleted_at, author, intent, shell, author_kind)
             values (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
            params![
                r.id,
                r.timestamp,
                r.duration,
                r.exit,
                r.command,
                r.cwd,
                r.session,
                r.hostname,
                r.deleted_at,
                r.author,
                r.intent,
                r.shell,
                r.author_kind,
            ],
        )
        .unwrap();
    }
    conn.execute_batch("PRAGMA wal_checkpoint(TRUNCATE);").ok();
}

/// Fingerprint of every file `SQLite` may have written for this database.
fn source_fingerprint(path: &Path) -> Vec<(String, Option<Vec<u8>>)> {
    ["", "-wal", "-shm"]
        .iter()
        .map(|suffix| {
            let p = PathBuf::from(format!("{}{suffix}", path.display()));
            (suffix.to_string(), std::fs::read(&p).ok())
        })
        .collect()
}

struct Sandbox {
    home: tempfile::TempDir,
}

impl Sandbox {
    fn new() -> Self {
        Self {
            home: tempfile::tempdir().unwrap(),
        }
    }

    fn run(&self, args: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_suv"))
            .args(args)
            .env("HOME", self.home.path())
            .env("XDG_DATA_HOME", self.home.path().join("data"))
            .env("XDG_CONFIG_HOME", self.home.path().join("config"))
            .env_remove("SUVADU_PAUSED")
            .env("NO_COLOR", "1")
            .output()
            .unwrap()
    }

    fn import(&self, file: &Path, extra: &[&str]) -> String {
        let out = self.import_raw(file, extra);
        assert!(
            out.status.success(),
            "import failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8(out.stdout).unwrap()
    }

    fn import_raw(&self, file: &Path, extra: &[&str]) -> Output {
        let path = file.to_str().unwrap();
        let mut args = vec!["import", "--from", "atuin-db"];
        args.extend_from_slice(extra);
        args.push(path);
        self.run(&args)
    }

    fn path(&self, name: &str) -> PathBuf {
        self.home.path().join(name)
    }

    fn history_json(&self) -> Vec<serde_json::Value> {
        let out = self.run(&["history", "--json", "-n", "200"]);
        assert!(
            out.status.success(),
            "history failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8(out.stdout)
            .unwrap()
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect()
    }
}

#[test]
fn atuin_import_cli_maps_metadata_and_is_idempotent() {
    let s = Sandbox::new();
    let db = s.path("history.db");
    let mut agent = Row::new(
        "id-agent",
        1_700_000_200_000_000_000,
        "cargo test --offline",
    );
    agent.author = Some("claude".to_string());
    agent.author_kind = Some(2);
    agent.intent = Some("verify the build".to_string());
    agent.exit = 101;
    agent.duration = 2_500_000_000;
    let mut unfinished = Row::new("id-unknown", 1_700_000_300_000_000_000, "sleep 100");
    unfinished.exit = -1;
    unfinished.duration = -1;
    unfinished.cwd = "unknown".to_string();
    unfinished.author_kind = None;
    atuin_db(
        &db,
        &[
            Row::new("id-uni", 1_700_000_100_000_000_000, "echo 'héllo 世界 🌍'"),
            Row::new(
                "id-multi",
                1_700_000_150_000_000_000,
                "for i in 1 2; do\n  echo $i\ndone",
            ),
            agent,
            unfinished,
        ],
    );
    let before = source_fingerprint(&db);

    let out = s.import(&db, &[]);
    assert!(out.contains("Imported: 4"), "{out}");
    assert!(out.contains("Atuin schema 20260818000000"), "{out}");

    let entries = s.history_json();
    assert_eq!(entries.len(), 4);
    let by_cmd = |needle: &str| -> serde_json::Value {
        entries
            .iter()
            .find(|e| e["command"].as_str().unwrap().contains(needle))
            .unwrap_or_else(|| panic!("no entry matching {needle} in {entries:#?}"))
            .clone()
    };

    let uni = by_cmd("héllo");
    assert_eq!(uni["command"].as_str().unwrap(), "echo 'héllo 世界 🌍'");
    assert_eq!(uni["started_at"].as_i64().unwrap(), 1_700_000_100_000);
    assert_eq!(uni["duration_ms"].as_i64().unwrap(), 1_500);
    assert_eq!(uni["exit_code"].as_i64().unwrap(), 0);
    assert_eq!(uni["cwd"].as_str().unwrap(), "/home/ellie/work");
    assert_eq!(uni["executor_type"].as_str().unwrap(), "human");

    let multi = by_cmd("for i in 1 2");
    assert_eq!(
        multi["command"].as_str().unwrap(),
        "for i in 1 2; do\n  echo $i\ndone"
    );

    let agent = by_cmd("cargo test");
    assert_eq!(agent["exit_code"].as_i64().unwrap(), 101);
    assert_eq!(agent["executor_type"].as_str().unwrap(), "agent");
    assert_eq!(agent["executor"].as_str().unwrap(), "claude");
    let ctx = &agent["context"];
    assert_eq!(ctx["import_source"].as_str().unwrap(), "atuin-db");
    assert_eq!(ctx["timestamp_source"].as_str().unwrap(), "file");
    assert_eq!(ctx["atuin_id"].as_str().unwrap(), "id-agent");
    assert_eq!(ctx["atuin_intent"].as_str().unwrap(), "verify the build");

    let unknown = by_cmd("sleep 100");
    assert!(
        unknown["exit_code"].is_null(),
        "atuin's -1 means unknown, never a fabricated success"
    );
    assert_eq!(unknown["duration_ms"].as_i64().unwrap(), 0);
    assert_eq!(unknown["cwd"].as_str().unwrap(), "");
    assert_eq!(unknown["executor_type"].as_str().unwrap(), "unknown");
    assert!(unknown["context"]["unknown_fields"]
        .as_str()
        .unwrap()
        .contains("exit_code"));

    // The source database is untouched, and a second import adds nothing.
    assert_eq!(source_fingerprint(&db), before, "source database changed");
    let second = s.import(&db, &[]);
    assert!(second.contains("Imported: 0"), "{second}");
    assert!(second.contains("Already present: 4"), "{second}");
    assert_eq!(s.history_json().len(), 4);
    assert_eq!(source_fingerprint(&db), before, "source database changed");
}

#[test]
fn atuin_import_cli_dry_run_reports_and_writes_nothing() {
    let s = Sandbox::new();
    let db = s.path("history.db");
    let mut deleted = Row::new("id-del", 1_700_000_400_000_000_000, "rm -rf secrets");
    deleted.deleted_at = Some(1_700_000_500_000_000_000);
    atuin_db(
        &db,
        &[
            Row::new("id-a", 1_700_000_100_000_000_000, "git status"),
            Row::new("id-b", 1_700_000_200_000_000_000, "ls -la"),
            deleted,
        ],
    );
    let before = source_fingerprint(&db);

    let out = s.import(&db, &["--dry-run"]);
    assert!(out.contains("2 entry(ies) would be imported"), "{out}");
    assert!(out.contains("git status"), "preview shows a sample: {out}");
    assert!(
        out.contains("Deleted in Atuin, skipped: 1"),
        "soft-deleted rows are reported, not imported: {out}"
    );
    assert!(
        !out.contains("rm -rf secrets"),
        "a skipped record is never echoed: {out}"
    );

    assert!(s.history_json().is_empty(), "dry run wrote entries");
    assert_eq!(source_fingerprint(&db), before, "source database changed");
}

#[test]
fn atuin_import_cli_rejects_an_untested_schema_version() {
    let s = Sandbox::new();
    let db = s.path("history.db");
    let mut migrations = MIGRATIONS_18_22.to_vec();
    migrations.push(20_270_101_000_000);
    atuin_db_with_migrations(
        &db,
        &migrations,
        &[Row::new("id-a", 1_700_000_100_000_000_000, "git status")],
    );

    let out = s.import_raw(&db, &[]);
    assert!(!out.status.success(), "an untested schema must be rejected");
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("20270101000000"), "{err}");
    assert!(
        err.contains("suv update"),
        "the rejection must give a next step: {err}"
    );
    assert!(s.history_json().is_empty(), "nothing was written");
}

#[test]
fn atuin_import_cli_backs_up_first_and_leaves_existing_records_intact() {
    let s = Sandbox::new();
    let bash = s.path(".bash_history");
    std::fs::write(&bash, b"#1600000000\necho pre-existing\n").unwrap();
    let out = s.run(&["import", "--from", "bash-history", bash.to_str().unwrap()]);
    assert!(out.status.success());

    let db = s.path("history.db");
    atuin_db(
        &db,
        &[Row::new("id-a", 1_700_000_100_000_000_000, "git status")],
    );
    let out = s.import(&db, &[]);

    assert!(out.contains("Backup:"), "{out}");
    let backup_line = out
        .lines()
        .find(|l| l.trim_start().starts_with("Backup:"))
        .unwrap();
    let backup = backup_line.split_once(':').unwrap().1.trim();
    assert!(
        Path::new(backup).exists(),
        "backup file {backup} does not exist"
    );
    assert!(out.contains("Rollback:"), "{out}");
    assert!(out.contains("Verified"), "post-import validation: {out}");

    let commands: Vec<String> = s
        .history_json()
        .iter()
        .map(|e| e["command"].as_str().unwrap().to_string())
        .collect();
    assert!(commands.contains(&"echo pre-existing".to_string()));
    assert!(commands.contains(&"git status".to_string()));
}

#[test]
fn atuin_import_cli_skips_malformed_rows_and_keeps_going() {
    let s = Sandbox::new();
    let db = s.path("history.db");
    atuin_db(
        &db,
        &[
            Row::new("id-a", 1_700_000_100_000_000_000, "git status"),
            Row::new("id-zero", 0, "date"),
        ],
    );
    // A timestamp SQLite stored as text — not a number we can trust.
    let conn = Connection::open(&db).unwrap();
    conn.execute(
        "insert into history (id, timestamp, duration, exit, command, cwd, session, hostname)
         values ('id-bad', 'not-a-timestamp', 0, 0, 'echo broken', '/tmp', 's', 'h:u')",
        [],
    )
    .unwrap();
    drop(conn);

    let out = s.import(&db, &[]);
    assert!(out.contains("Malformed rows skipped: 2"), "{out}");
    assert!(out.contains("Imported: 1"), "{out}");
    assert!(
        !out.contains("echo broken"),
        "a malformed record is never echoed: {out}"
    );
}

/// Shaped like a GitHub token, never issued.
const FAKE_TOKEN: &str = "ghp_abcdefghijklmnopqrstuvwxyz0123";

#[test]
fn atuin_import_cli_redacts_a_secret_that_only_appears_in_metadata() {
    let s = Sandbox::new();
    let db = s.path("history.db");
    let mut row = Row::new("id-meta", 1_700_000_100_000_000_000, "echo normal");
    row.intent = Some(format!("deploy with GITHUB_TOKEN={FAKE_TOKEN}"));
    atuin_db(&db, &[row]);

    let out = s.import(&db, &[]);
    assert!(out.contains("Imported: 1"), "{out}");
    assert!(
        out.contains("Redacted before storage: 1"),
        "a metadata-only secret is counted, not silently kept: {out}"
    );
    assert!(
        !out.contains(FAKE_TOKEN),
        "the report echoed the secret: {out}"
    );

    let entries = s.history_json();
    assert_eq!(entries.len(), 1);
    let intent = entries[0]["context"]["atuin_intent"].as_str().unwrap();
    assert!(!intent.contains(FAKE_TOKEN), "{intent}");
    assert!(intent.contains("REDACTED"), "{intent}");
}

#[test]
fn atuin_import_cli_uses_each_source_directorys_project_policy() {
    let s = Sandbox::new();
    let project = s.path("project");
    std::fs::create_dir_all(&project).unwrap();
    std::fs::write(
        project.join(".suvadu.toml"),
        "exclusions = [\"PRIVATEPROJECT\"]\n\
         [redaction]\nextra_patterns = [\"corp-[a-z0-9]{6}\"]\n",
    )
    .unwrap();
    let dir = project.to_string_lossy().to_string();

    let db = s.path("history.db");
    let mut secret = Row::new(
        "id-secret",
        1_700_000_100_000_000_000,
        "deploy --key corp-ab12cd",
    );
    secret.cwd.clone_from(&dir);
    let mut excluded = Row::new(
        "id-excluded",
        1_700_000_200_000_000_000,
        "echo PRIVATEPROJECT",
    );
    excluded.cwd.clone_from(&dir);
    atuin_db(&db, &[secret, excluded]);

    let out = s.import(&db, &[]);
    assert!(out.contains("Excluded by config: 1"), "{out}");

    let commands: Vec<String> = s
        .history_json()
        .iter()
        .map(|e| e["command"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(commands.len(), 1, "{commands:?}");
    assert!(!commands[0].contains("corp-ab12cd"), "{commands:?}");
    assert!(commands[0].contains("REDACTED"), "{commands:?}");
}

#[test]
fn atuin_import_cli_keeps_executions_that_collide_after_truncation() {
    let s = Sandbox::new();
    let db = s.path("history.db");
    atuin_db(
        &db,
        &[
            Row::new("0", 1_700_000_000_000_000_000, "true"),
            Row::new("1", 1_700_000_000_000_000_001, "true"),
            Row::new("2", 1_700_000_000_001_000_000, "true"),
        ],
    );

    let preview = s.import(&db, &["--dry-run"]);
    assert!(
        preview.contains("3 entry(ies) would be imported"),
        "{preview}"
    );

    let out = s.import(&db, &[]);
    assert!(
        out.contains("Imported: 3"),
        "the apply matches the preview: {out}"
    );
    assert!(out.contains("Already present: 0"), "{out}");

    let entries = s.history_json();
    let mut ids: Vec<String> = entries
        .iter()
        .map(|e| e["context"]["atuin_id"].as_str().unwrap().to_string())
        .collect();
    ids.sort();
    assert_eq!(ids, vec!["0", "1", "2"], "every source row survives");

    let second = s.import(&db, &[]);
    assert!(second.contains("Imported: 0"), "{second}");
    assert!(second.contains("Already present: 3"), "{second}");
    assert_eq!(s.history_json().len(), 3);
}
