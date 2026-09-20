//! End-to-end tests for `suv import --from bash-history`.
//!
//! Each test runs the real binary against a private HOME and a temporary
//! fixture file — never the user's own database or history.
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

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
        let path = file.to_str().unwrap();
        let mut args = vec!["import", "--from", "bash-history"];
        args.extend_from_slice(extra);
        args.push(path);
        let out = self.run(&args);
        assert!(
            out.status.success(),
            "import failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8(out.stdout).unwrap()
    }

    fn fixture(&self, name: &str, contents: &[u8]) -> PathBuf {
        let path = self.home.path().join(name);
        std::fs::write(&path, contents).unwrap();
        path
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
fn bash_import_cli_preserves_timestamps_and_is_idempotent() {
    let s = Sandbox::new();
    let file = s.fixture(
        ".bash_history",
        b"#1700000000\ngit status\n#1700000100\nfor i in 1 2; do\n  echo $i\ndone\n",
    );

    let first = s.import(&file, &[]);
    assert!(first.contains("timestamped format"), "{first}");
    assert!(first.contains("Imported: 2"), "{first}");

    let entries = s.history_json();
    assert_eq!(entries.len(), 2);
    let multiline = entries
        .iter()
        .find(|e| e["command"].as_str().unwrap().starts_with("for i"))
        .expect("multiline record");
    assert_eq!(
        multiline["command"].as_str().unwrap(),
        "for i in 1 2; do\n  echo $i\ndone",
        "the #<epoch> header is the record boundary"
    );
    assert_eq!(multiline["started_at"].as_i64().unwrap(), 1_700_000_100_000);
    assert!(multiline["exit_code"].is_null(), "never fabricate success");
    assert_eq!(multiline["cwd"].as_str().unwrap(), "");

    // Second identical import adds nothing.
    let second = s.import(&file, &[]);
    assert!(second.contains("Imported: 0"), "{second}");
    assert!(second.contains("Already present: 2"), "{second}");
    assert_eq!(s.history_json().len(), 2);
}

#[test]
fn bash_import_cli_dry_run_writes_nothing_and_never_touches_the_input() {
    let s = Sandbox::new();
    let raw: &[u8] = b"#1700000000\ngit status\n#1700000100\nls -la\n";
    let file = s.fixture(".bash_history", raw);
    let before = std::fs::metadata(&file).unwrap().modified().unwrap();

    let out = s.import(&file, &["--dry-run"]);
    assert!(out.contains("2 entry(ies) would be imported"), "{out}");
    assert!(out.contains("git status"), "preview shows a sample: {out}");

    assert!(s.history_json().is_empty(), "dry run wrote entries");
    assert_eq!(
        std::fs::read(&file).unwrap(),
        raw,
        "input file was modified"
    );
    assert_eq!(
        std::fs::metadata(&file).unwrap().modified().unwrap(),
        before,
        "input file mtime changed"
    );
}

#[test]
fn bash_import_cli_plain_file_explains_its_limits_and_never_invents_times() {
    let s = Sandbox::new();
    let file = s.fixture(".bash_history", b"echo hello\nls\nls\n");

    let out = s.import(&file, &[]);
    assert!(out.contains("plain format"), "{out}");
    assert!(
        out.contains("multi-line commands cannot be reconstructed"),
        "the limitation must be stated: {out}"
    );
    assert!(out.contains("Imported: 3"), "repeats are kept: {out}");

    for entry in s.history_json() {
        let started = entry["started_at"].as_i64().unwrap();
        assert!(
            started < 86_400_000,
            "synthetic sentinel timestamp expected, got {started}"
        );
    }

    // Re-import adds nothing even without timestamps in the file.
    let again = s.import(&file, &[]);
    assert!(again.contains("Imported: 0"), "{again}");
    assert_eq!(s.history_json().len(), 3);
}

#[test]
fn bash_import_cli_handles_empty_malformed_and_non_utf8_files() {
    let s = Sandbox::new();

    let empty = s.fixture("empty_history", b"");
    let out = s.import(&empty, &[]);
    assert!(out.contains("Parsed 0 command(s)"), "{out}");
    assert!(out.contains("Imported: 0"), "{out}");
    assert!(s.history_json().is_empty());

    let mut bytes = b"#1700000000\n#99999999999999999999\necho ".to_vec();
    bytes.push(0xFF);
    bytes.extend_from_slice(b"\n");
    let messy = s.fixture("messy_history", &bytes);
    let out = s.import(&messy, &[]);
    assert!(out.contains("Malformed records skipped: 2"), "{out}");
    assert!(out.contains("invalid UTF-8"), "{out}");
    assert!(out.contains("Imported: 1"), "{out}");
}

#[test]
fn bash_import_cli_redacts_secrets_without_printing_them() {
    let s = Sandbox::new();
    let secret = "ghp_abcdefghijklmnopqrstuvwxyz0123456789";
    let file = s.fixture(
        ".bash_history",
        format!("#1700000000\nexport GITHUB_TOKEN={secret}\n").as_bytes(),
    );

    let out = s.import(&file, &["--dry-run"]);
    assert!(!out.contains(secret), "dry-run output leaked a secret");

    let out = s.import(&file, &[]);
    assert!(!out.contains(secret), "import output leaked a secret");
    assert!(out.contains("Redacted before storage: 1"), "{out}");

    let stored = s.history_json();
    assert_eq!(stored.len(), 1);
    let command = stored[0]["command"].as_str().unwrap();
    assert!(!command.contains(secret), "secret reached storage");
    assert!(command.contains("REDACTED"));
}
