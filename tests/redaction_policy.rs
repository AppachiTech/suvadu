//! One privacy policy, every way data gets in.
//!
//! A user configures `exclusions` and `redaction` once. This exercises each
//! ingestion path against the *same* configuration — live recording, the
//! Bash importer, the Zsh importer and native transcript ingestion — so a
//! path that quietly ignores the policy is visible instead of assumed.
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

/// Known gap, pinned rather than assumed: unlike live recording and the Bash
/// importer, the Zsh importer stores what the file says. A `~/.zsh_history`
/// that already contains secrets is imported verbatim, and `exclusions` are
/// not consulted. SECURITY.md documents this; if it is ever fixed, this test
/// is the thing that says so.
#[test]
fn zsh_import_does_not_apply_redaction_or_exclusions_today() {
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
        stored.contains("ghp_abcdefghijklmnopqrstuvwxyz0123"),
        "zsh import gained redaction — update SECURITY.md and this test:\n{stored}"
    );
    assert!(
        stored.contains("vault login"),
        "zsh import gained exclusion support — update SECURITY.md and this test:\n{stored}"
    );
    // The one rule it does share with every other path.
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
