//! `suv status` and `suv doctor` must never present *stored* history as proof
//! that the shell hook actually captured anything.
//!
//! Importing a history file writes rows dated whenever the source file says —
//! including "now". Those rows are searchable history, but nothing about them
//! shows that a live hook is running. Every test here runs the real binary
//! against a private HOME with its own database; the user's own database and
//! config are never opened.
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use rusqlite::params;

/// The `_sqlx_migrations` rows an Atuin 18.22.0 `history.db` carries.
const ATUIN_MIGRATIONS: &[i64] = &[
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

struct Sandbox {
    home: tempfile::TempDir,
}

impl Sandbox {
    fn new() -> Self {
        Self {
            home: tempfile::tempdir().unwrap(),
        }
    }

    fn path(&self, name: &str) -> PathBuf {
        self.home.path().join(name)
    }

    /// Run the CLI as if it were invoked from a shell with `session`
    /// exported (`None` = no `SUVADU_SESSION_ID`, like a bare subprocess).
    fn run_in(&self, session: Option<&str>, args: &[&str]) -> Output {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_suv"));
        cmd.args(args)
            .env("HOME", self.home.path())
            .env("XDG_DATA_HOME", self.home.path().join("data"))
            .env("XDG_CONFIG_HOME", self.home.path().join("config"))
            .env("SHELL", "/bin/zsh")
            .env_remove("SUVADU_PAUSED")
            .env_remove("SUVADU_SESSION_ID")
            .env("NO_COLOR", "1");
        if let Some(session) = session {
            cmd.env("SUVADU_SESSION_ID", session);
        }
        cmd.output().unwrap()
    }

    fn ok(&self, session: Option<&str>, args: &[&str]) -> String {
        let out = self.run_in(session, args);
        assert!(
            out.status.success(),
            "{args:?} failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8(out.stdout).unwrap()
    }

    fn status(&self, session: Option<&str>) -> String {
        self.ok(session, &["status"])
    }

    fn doctor(&self, session: Option<&str>) -> String {
        self.ok(session, &["doctor"])
    }

    /// Install the zsh hook the way `suv init zsh` tells the user to.
    fn install_zsh_hook(&self) {
        std::fs::write(self.path(".zshrc"), "eval \"$(suv init zsh)\"\n").unwrap();
    }

    /// One command recorded the way the live shell hook records it.
    fn record_live(&self, session: &str, command: &str) {
        self.record_live_at(session, command, chrono::Utc::now().timestamp_millis());
    }

    /// `record_live`, but at a chosen time, for "this record is old" cases.
    fn record_live_at(&self, session: &str, command: &str, at_ms: i64) {
        let now = at_ms.to_string();
        self.ok(
            Some(session),
            &[
                "add",
                "--session-id",
                session,
                "--command",
                command,
                "--cwd",
                "/work",
                "--exit-code",
                "0",
                "--started-at",
                &now,
                "--ended-at",
                &now,
                "--executor-type",
                "human",
                "--executor",
                "terminal",
            ],
        );
    }

    fn import_bash(&self) {
        let file = self.path("bash_history");
        std::fs::write(
            &file,
            format!("#{}\necho imported-not-captured\n", now_secs()),
        )
        .unwrap();
        self.ok(
            None,
            &["import", "--from", "bash-history", file.to_str().unwrap()],
        );
    }

    fn import_zsh(&self) {
        let file = self.path("zsh_history");
        std::fs::write(
            &file,
            format!(": {}:0;echo imported-not-captured\n", now_secs()),
        )
        .unwrap();
        self.ok(
            None,
            &["import", "--from", "zsh-history", file.to_str().unwrap()],
        );
    }

    fn import_jsonl(&self) {
        let file = self.path("export.jsonl");
        let now = chrono::Utc::now().timestamp_millis();
        std::fs::write(
            &file,
            format!(
                "{{\"session_id\":\"other-machine-session\",\"command\":\"echo imported-not-captured\",\
                 \"cwd\":\"/work\",\"exit_code\":0,\"started_at\":{now},\"ended_at\":{now},\"duration_ms\":0}}\n"
            ),
        )
        .unwrap();
        self.ok(None, &["import", file.to_str().unwrap()]);
    }

    fn import_atuin(&self) {
        let file = self.path("atuin.db");
        atuin_db(&file, now_secs() * 1_000_000_000);
        self.ok(
            None,
            &["import", "--from", "atuin-db", file.to_str().unwrap()],
        );
    }
}

fn now_secs() -> i64 {
    chrono::Utc::now().timestamp()
}

/// A minimal Atuin-shaped source database holding one recent command.
fn atuin_db(path: &Path, timestamp_ns: i64) {
    let conn = rusqlite::Connection::open(path).unwrap();
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
    for version in ATUIN_MIGRATIONS {
        conn.execute(
            "insert into _sqlx_migrations (version, description, success, checksum, execution_time)
             values (?1, 'test', 1, x'00', 0)",
            params![version],
        )
        .unwrap();
    }
    conn.execute(
        "insert into history (id, timestamp, duration, exit, command, cwd, session, hostname,
                              deleted_at, author, intent, shell, author_kind)
         values ('row-1', ?1, 1500000000, 0, 'echo imported-not-captured', '/work',
                 '0193c0ffee', 'laptop:ellie', NULL, NULL, NULL, 'bash', 1)",
        params![timestamp_ns],
    )
    .unwrap();
    conn.execute_batch("PRAGMA wal_checkpoint(TRUNCATE);").ok();
}

/// The `Capture:` line of `suv status`.
fn status_capture(status: &str) -> String {
    status
        .lines()
        .find(|l| l.trim_start().starts_with("Capture:"))
        .unwrap_or_else(|| panic!("no Capture line in status:\n{status}"))
        .to_string()
}

/// The `Capture evidence` row of `suv doctor`.
fn doctor_capture(doctor: &str) -> String {
    doctor
        .lines()
        .find(|l| l.contains("Capture evidence"))
        .unwrap_or_else(|| panic!("no Capture evidence row in doctor:\n{doctor}"))
        .to_string()
}

fn assert_capture_claimed(case: &str, sandbox: &Sandbox, session: Option<&str>) {
    let status = sandbox.status(session);
    let line = status_capture(&status);
    assert!(
        line.contains('\u{2705}'),
        "{case}: status should report verified capture, said: {line}"
    );
    let doctor = sandbox.doctor(session);
    let row = doctor_capture(&doctor);
    assert!(
        row.contains('\u{2713}'),
        "{case}: doctor should pass capture evidence, said: {row}"
    );
}

fn assert_capture_not_claimed(case: &str, sandbox: &Sandbox, session: Option<&str>) {
    let status = sandbox.status(session);
    let line = status_capture(&status);
    assert!(
        !line.contains('\u{2705}'),
        "{case}: status claimed verified capture, said: {line}"
    );
    let doctor = sandbox.doctor(session);
    let row = doctor_capture(&doctor);
    assert!(
        row.contains('\u{26a0}'),
        "{case}: doctor passed capture evidence, said: {row}"
    );
}

/// The reviewer's reproduction, for every importer: a single row dated *now*
/// arrives through an import, and neither surface may call that capture.
#[test]
fn imported_history_is_never_reported_as_captured() {
    for (name, import) in [
        ("bash-history", &Sandbox::import_bash as &dyn Fn(&Sandbox)),
        ("zsh-history", &Sandbox::import_zsh),
        ("jsonl", &Sandbox::import_jsonl),
        ("atuin-db", &Sandbox::import_atuin),
    ] {
        let sandbox = Sandbox::new();
        assert_capture_not_claimed(&format!("{name}: before import"), &sandbox, None);
        import(&sandbox);
        assert_capture_not_claimed(&format!("{name}: after import"), &sandbox, None);

        let status = sandbox.status(None);
        assert!(
            status.to_lowercase().contains("import"),
            "{name}: status must say the stored rows came from an import:\n{status}"
        );
        assert!(
            status.contains("suv init zsh"),
            "{name}: status must tell an import-only user how to start capturing:\n{status}"
        );
    }
}

/// A row written by the live hook, in the shell session being diagnosed, is
/// the thing that does prove capture.
#[test]
fn a_live_recorded_command_proves_capture() {
    let sandbox = Sandbox::new();
    sandbox.install_zsh_hook();
    sandbox.record_live("live-session", "echo hello");
    assert_capture_claimed("live record", &sandbox, Some("live-session"));
}

/// A live record from another session still counts while the shell being
/// diagnosed has the hook installed — a new terminal has not typed anything
/// yet, and that is not a broken setup.
#[test]
fn a_live_record_from_another_session_counts_while_this_shell_is_hooked() {
    let sandbox = Sandbox::new();
    sandbox.install_zsh_hook();
    sandbox.record_live("earlier-session", "echo hello");
    assert_capture_claimed("other session, hook installed", &sandbox, Some("fresh"));
}

/// …but when the shell being diagnosed has no hook at all, a record some
/// other shell produced says nothing about this one. This is exactly the
/// combination `suv doctor` used to report as "Capture evidence ✓" while
/// warning on the same screen that `~/.zshrc` was missing.
#[test]
fn a_record_from_another_shell_is_not_proof_for_an_unhooked_shell() {
    let sandbox = Sandbox::new();
    // No ~/.zshrc at all; the record came from a shell we are not diagnosing.
    sandbox.record_live("bash-session", "echo hello");
    assert_capture_not_claimed("other shell, no hook here", &sandbox, Some("zsh-session"));

    let status = sandbox.status(Some("zsh-session"));
    assert!(
        status.contains("suv init zsh"),
        "the unhooked shell must be told how to install its own hook:\n{status}"
    );
}

/// The documented route out of "not verified": install the hook, run the
/// marker command, and the very same diagnostics turn green.
#[test]
fn an_import_only_setup_becomes_verified_after_a_genuine_capture() {
    let sandbox = Sandbox::new();
    sandbox.import_bash();
    assert_capture_not_claimed("import only", &sandbox, Some("zsh-session"));

    let status = sandbox.status(Some("zsh-session"));
    assert!(
        status.contains("echo suvadu-capture-check"),
        "the verification sequence must be offered:\n{status}"
    );

    sandbox.install_zsh_hook();
    sandbox.record_live("zsh-session", "echo suvadu-capture-check");
    assert_capture_claimed("after a genuine capture", &sandbox, Some("zsh-session"));
}

/// F03: restoring a JSONL export into a session that already exists.
///
/// The importer only stamps its placeholder hostname on a session it has to
/// create, so a row restored into an existing session carried no import
/// marker at all and read as a locally captured record. Reproduced from the
/// corrective-release review: an old live record, then a JSONL row dated now
/// with the same session id, flipped capture to verified with no hook
/// installed.
#[test]
fn a_jsonl_import_into_an_existing_session_is_not_capture_evidence() {
    let sandbox = Sandbox::new();
    let session = "11111111-1111-4111-8111-111111111111";

    // A genuine hook-written record, but two days old: not current proof.
    let old = chrono::Utc::now().timestamp_millis() - 2 * 24 * 60 * 60 * 1000;
    sandbox.record_live_at(session, "echo captured-two-days-ago", old);
    assert_capture_not_claimed("before the import", &sandbox, Some(session));

    // Restoring an export into that same session must not become proof.
    let file = sandbox.path("export.jsonl");
    let now = chrono::Utc::now().timestamp_millis();
    std::fs::write(
        &file,
        format!(
            "{{\"session_id\":\"{session}\",\"command\":\"echo restored-not-captured\",\
             \"cwd\":\"/work\",\"exit_code\":0,\"started_at\":{now},\"ended_at\":{now},\
             \"duration_ms\":0}}\n"
        ),
    )
    .unwrap();
    sandbox.ok(Some(session), &["import", file.to_str().unwrap()]);

    assert_capture_not_claimed(
        "after restoring into an existing session",
        &sandbox,
        Some(session),
    );
}
