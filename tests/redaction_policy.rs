//! One privacy policy, every way data gets in.
//!
//! A user configures `exclusions` and `redaction` once. This exercises each
//! ingestion path against the *same* configuration — live recording, the
//! Bash importer, the Zsh importer, the Atuin importer and native transcript
//! ingestion — so a path that quietly ignores the policy is visible instead
//! of assumed. For a path that carries free text beside the command (an
//! agent prompt, an Atuin `intent`), that text is judged too.
//!
//! Every test runs the real binary against a private HOME; nothing here
//! reads or writes the user's own database, backups or config.

use std::path::PathBuf;
use std::process::{Command, Output};

/// The configuration under test, shared by every path below.
const CONFIG: &str = r#"
exclusions = ["^vault ", "SECRETFILE"]

[redaction]
enabled = true
extra_patterns = ["corp-[a-z0-9]{6}"]
"#;

/// A well-known secret shape (`ghp_…`) inside an env assignment.
const SECRET_COMMAND: &str = "export GITHUB_TOKEN=ghp_abcdefghijklmnopqrstuvwxyz0123";
/// A secret only the user's own `extra_patterns` knows about.
const CUSTOM_SECRET_COMMAND: &str = "deploy --key corp-ab12cd";
/// Matches an exclusion pattern: must never reach the database at all.
const EXCLUDED_COMMAND: &str = "vault login -method=okta";

struct Sandbox {
    home: tempfile::TempDir,
}

impl Sandbox {
    fn new() -> Self {
        let sandbox = Self {
            home: tempfile::tempdir().unwrap(),
        };
        let config = sandbox.config_path();
        std::fs::create_dir_all(config.parent().unwrap()).unwrap();
        std::fs::write(&config, CONFIG).unwrap();
        sandbox
    }

    /// Where `directories::ProjectDirs::from("tech", "appachi", "suvadu")`
    /// puts the config file for this platform, under the sandbox's HOME.
    fn config_path(&self) -> PathBuf {
        let home = self.home.path();
        if cfg!(target_os = "macos") {
            home.join("Library/Application Support/tech.appachi.suvadu/config.toml")
        } else {
            home.join("config/suvadu/config.toml")
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

    fn stdout(&self, args: &[&str]) -> String {
        let out = self.run(args);
        assert!(
            out.status.success(),
            "`suv {}` failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8(out.stdout).unwrap()
    }

    fn fixture(&self, name: &str, contents: &str) -> PathBuf {
        let path = self.home.path().join(name);
        std::fs::write(&path, contents).unwrap();
        path
    }

    /// Record one command the way a shell hook does.
    fn record(&self, command: &str, at: i64) {
        let at = at.to_string();
        self.stdout(&[
            "add",
            "--session-id",
            "shell-1",
            "--command",
            command,
            "--cwd",
            "/work/project",
            "--exit-code",
            "0",
            "--started-at",
            &at,
            "--ended-at",
            &at,
        ]);
    }

    fn stored_commands(&self) -> Vec<String> {
        self.stdout(&["history", "--json", "-n", "100"])
            .lines()
            .map(|line| {
                serde_json::from_str::<serde_json::Value>(line).unwrap()["command"]
                    .as_str()
                    .unwrap()
                    .to_string()
            })
            .collect()
    }
}

/// The configured policy must be applied as commands are recorded, because
/// there is no second chance: redaction never rewrites stored history.
#[test]
fn live_recording_redacts_known_and_custom_secrets_and_drops_excluded_commands() {
    let s = Sandbox::new();
    s.record(SECRET_COMMAND, 1_700_000_000_000);
    s.record(CUSTOM_SECRET_COMMAND, 1_700_000_001_000);
    s.record(EXCLUDED_COMMAND, 1_700_000_002_000);
    s.record(
        " quiet --token ghp_abcdefghijklmnopqrstuvwxyz0123",
        1_700_000_003_000,
    );

    let stored = s.stored_commands().join("\n");

    assert!(
        !stored.contains("ghp_abcdefghijklmnopqrstuvwxyz0123"),
        "a known secret shape reached storage:\n{stored}"
    );
    assert!(
        stored.contains("REDACTED"),
        "nothing was redacted:\n{stored}"
    );
    assert!(
        !stored.contains("corp-ab12cd"),
        "a user-configured secret pattern was ignored:\n{stored}"
    );
    assert!(
        !stored.contains("vault login"),
        "an excluded command was stored:\n{stored}"
    );
    assert!(
        !stored.contains("quiet"),
        "a space-prefixed command was stored:\n{stored}"
    );
}

/// The Bash importer is an ingestion path like any other, and applies the
/// same three rules: exclusion, redaction, space-prefix.
#[test]
fn bash_import_applies_exclusions_redaction_and_the_space_prefix_rule() {
    let s = Sandbox::new();
    let file = s.fixture(
        "bash_history",
        &format!(
            "#1700000000\n{SECRET_COMMAND}\n#1700000060\n{CUSTOM_SECRET_COMMAND}\n\
             #1700000120\n{EXCLUDED_COMMAND}\n#1700000180\n cat SECRETFILE\n"
        ),
    );

    let report = s.stdout(&["import", "--from", "bash-history", file.to_str().unwrap()]);
    assert!(report.contains("Redacted before storage: 2"), "{report}");

    let stored = s.stored_commands().join("\n");
    assert!(
        !stored.contains("ghp_abcdefghijklmnopqrstuvwxyz0123"),
        "{stored}"
    );
    assert!(!stored.contains("corp-ab12cd"), "{stored}");
    assert!(!stored.contains("vault login"), "{stored}");
    assert!(!stored.contains("SECRETFILE"), "{stored}");
}

/// A dry run must not leak what the real import would have hidden.
#[test]
fn bash_import_preview_shows_redacted_text_only() {
    let s = Sandbox::new();
    let file = s.fixture("bash_history", &format!("#1700000000\n{SECRET_COMMAND}\n"));
    let preview = s.stdout(&[
        "import",
        "--from",
        "bash-history",
        "--dry-run",
        file.to_str().unwrap(),
    ]);
    assert!(
        !preview.contains("ghp_abcdefghijklmnopqrstuvwxyz0123"),
        "the preview printed a secret:\n{preview}"
    );
}

/// The Zsh importer applies the same policy as live recording and the Bash
/// importer: a secret in `~/.zsh_history` is redacted before storage, and an
/// excluded command is not imported at all.
#[test]
fn zsh_import_redacts_secrets_and_honours_exclusions() {
    let s = Sandbox::new();
    let file = s.fixture(
        "zsh_history",
        &format!(
            ": 1700000000:0;{SECRET_COMMAND}\n: 1700000060:0;{EXCLUDED_COMMAND}\n\
             : 1700000120:0; quiet secret\n"
        ),
    );
    s.stdout(&["import", "--from", "zsh-history", file.to_str().unwrap()]);

    let stored = s.stored_commands().join("\n");
    assert!(
        !stored.contains("ghp_abcdefghijklmnopqrstuvwxyz0123"),
        "an imported secret must be redacted before storage:\n{stored}"
    );
    assert!(
        !stored.contains("vault login"),
        "an excluded command must not be imported:\n{stored}"
    );
    assert!(
        !stored.contains("quiet secret"),
        "space-prefixed history lines must never be imported:\n{stored}"
    );
}

/// Ingesting a native agent transcript applies the same policy to prompt and
/// answer text that recording applies to commands.
#[test]
fn transcript_ingestion_redacts_prompts_and_drops_excluded_turns() {
    let s = Sandbox::new();
    let transcript = s.fixture(
        "codex.jsonl",
        &[
            r#"{"timestamp":"2026-09-12T12:00:00Z","type":"session_meta","payload":{"id":"native-9","cwd":"/work/project"}}"#,
            r#"{"timestamp":"2026-09-12T12:00:01Z","type":"turn_context","payload":{"turn_id":"t1","cwd":"/work/project","model":"m"}}"#,
            r#"{"timestamp":"2026-09-12T12:00:02Z","type":"event_msg","payload":{"type":"user_message","message":"use GITHUB_TOKEN=ghp_abcdefghijklmnopqrstuvwxyz0123 and key corp-ab12cd"}}"#,
            r#"{"timestamp":"2026-09-12T12:00:03Z","type":"event_msg","payload":{"type":"user_message","message":"read SECRETFILE for me"}}"#,
            r#"{"timestamp":"2026-09-12T12:00:04Z","type":"event_msg","payload":{"type":"agent_message","phase":"final_answer","message":"Done."}}"#,
        ]
        .join("\n"),
    );

    s.stdout(&["agent", "import-session", transcript.to_str().unwrap()]);
    let session = s.stdout(&["agent", "session", "codex-native-9"]);

    assert!(
        !session.contains("ghp_abcdefghijklmnopqrstuvwxyz0123"),
        "a secret survived transcript ingestion:\n{session}"
    );
    assert!(
        !session.contains("corp-ab12cd"),
        "a user-configured secret pattern was ignored on ingestion:\n{session}"
    );
    assert!(
        !session.contains("SECRETFILE"),
        "an excluded turn was captured:\n{session}"
    );
    // Redaction edits the turn; it does not discard it.
    assert!(
        session.contains("use GITHUB_TOKEN=***REDACTED*** and key ***REDACTED***"),
        "a redacted turn must still be captured, minus the secrets:\n{session}"
    );
}

/// Build an Atuin 18.22.0-shaped `history.db` beneath the sandbox.
///
/// Columns: `(id, timestamp_ns, command, cwd, intent)`. Nothing here reads
/// the user's own Atuin database.
fn atuin_fixture(s: &Sandbox, rows: &[(&str, i64, &str, &str, Option<&str>)]) -> PathBuf {
    let path = s.home.path().join("atuin-history.db");
    let conn = rusqlite::Connection::open(&path).unwrap();
    conn.execute_batch(
        "create table history (
            id text primary key, timestamp integer not null, duration integer not null,
            exit integer not null, command text not null, cwd text not null,
            session text not null, hostname text not null, deleted_at integer,
            author text, intent text, shell text, author_kind integer);
         create table _sqlx_migrations (
            version bigint primary key, description text not null,
            installed_on timestamp not null default current_timestamp,
            success boolean not null, checksum blob not null, execution_time bigint not null);",
    )
    .unwrap();
    for v in [
        20_210_422_143_411_i64,
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
    ] {
        conn.execute(
            "insert into _sqlx_migrations (version, description, success, checksum, \
             execution_time) values (?1, 'test', 1, x'00', 0)",
            rusqlite::params![v],
        )
        .unwrap();
    }
    for (id, ts, command, cwd, intent) in rows {
        conn.execute(
            "insert into history (id, timestamp, duration, exit, command, cwd, session, \
             hostname, author, intent, shell, author_kind)
             values (?1, ?2, 0, 0, ?3, ?4, 'atuin-session', 'laptop:ellie', 'claude', ?5, \
             'zsh', 2)",
            rusqlite::params![id, ts, command, cwd, intent],
        )
        .unwrap();
    }
    conn.execute_batch("PRAGMA wal_checkpoint(TRUNCATE);").ok();
    path
}

/// The Atuin importer is an ingestion path like any other. Atuin also stores
/// free-form `intent` text beside the command, and that text is governed by
/// the same policy — a secret an agent wrote into its intent must not reach
/// storage just because it never appeared on a command line.
#[test]
fn atuin_import_applies_the_policy_to_commands_and_to_free_text_metadata() {
    let s = Sandbox::new();
    let db = atuin_fixture(
        &s,
        &[
            (
                "r1",
                1_700_000_000_000_000_000,
                SECRET_COMMAND,
                "/work",
                None,
            ),
            (
                "r2",
                1_700_000_001_000_000_000,
                CUSTOM_SECRET_COMMAND,
                "/work",
                None,
            ),
            (
                "r3",
                1_700_000_002_000_000_000,
                EXCLUDED_COMMAND,
                "/work",
                None,
            ),
            (
                "r4",
                1_700_000_003_000_000_000,
                "echo normal",
                "/work",
                Some("deploy with GITHUB_TOKEN=ghp_abcdefghijklmnopqrstuvwxyz0123 and corp-ab12cd"),
            ),
            (
                "r5",
                1_700_000_004_000_000_000,
                "echo also normal",
                "/work",
                Some("go and read SECRETFILE"),
            ),
        ],
    );

    let report = s.stdout(&["import", "--from", "atuin-db", db.to_str().unwrap()]);
    assert!(
        report.contains("Excluded by config: 1"),
        "the excluded command was imported: {report}"
    );
    assert!(
        report.contains("Redacted before storage: 4 row(s); 2 metadata field(s)"),
        "two secret commands plus two rows whose only offending text was in \
         `intent` (one redacted, one withheld): {report}"
    );

    let everything = s.stdout(&["history", "--json", "-n", "100"]);
    assert!(
        !everything.contains("ghp_abcdefghijklmnopqrstuvwxyz0123"),
        "a known secret shape reached storage:\n{everything}"
    );
    assert!(
        !everything.contains("corp-ab12cd"),
        "a user-configured pattern was ignored:\n{everything}"
    );
    assert!(
        !everything.contains("vault login"),
        "an excluded command was stored:\n{everything}"
    );
    assert!(
        !everything.contains("SECRETFILE"),
        "excluded text was stored in metadata:\n{everything}"
    );
    // Redaction edits the metadata; it does not discard the row.
    assert!(
        everything.contains("echo normal"),
        "a clean command was dropped:\n{everything}"
    );
    assert!(
        everything.contains("withheld_fields"),
        "a withheld metadata field must be declared, not silently missing:\n{everything}"
    );
}

/// A dry run must reach the same verdict and leak nothing the apply hides.
#[test]
fn atuin_import_preview_shows_redacted_text_only() {
    let s = Sandbox::new();
    let db = atuin_fixture(
        &s,
        &[(
            "r1",
            1_700_000_000_000_000_000,
            "echo normal",
            "/work",
            Some("deploy with GITHUB_TOKEN=ghp_abcdefghijklmnopqrstuvwxyz0123"),
        )],
    );
    let preview = s.stdout(&[
        "import",
        "--from",
        "atuin-db",
        "--dry-run",
        db.to_str().unwrap(),
    ]);
    assert!(
        !preview.contains("ghp_abcdefghijklmnopqrstuvwxyz0123"),
        "the preview printed a secret:\n{preview}"
    );
    assert!(
        preview.contains("Redacted before storage: 1"),
        "the dry run must count what the apply would redact:\n{preview}"
    );
}
