//! End-to-end tests for the suvadu-session-memory builtin skill: seeded
//! and materialized by both `suv init claude-code` and `suv skills sync`.
//! Uses a private home and real CLI/database, same approach as
//! `codex_hooks.rs`/`opencode_hooks.rs`.
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
    fn command(&self) -> Command {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_suv"));
        cmd.current_dir(self.home.path())
            .env("HOME", self.home.path())
            .env("XDG_DATA_HOME", self.home.path().join("data"))
            .env("XDG_CONFIG_HOME", self.home.path().join("config"))
            .env_remove("SUVADU_PAUSED")
            .env("NO_COLOR", "1");
        cmd
    }
    fn run(&self, args: &[&str]) -> Output {
        self.command().args(args).output().unwrap()
    }
    /// Ensures config.toml exists (via `suv enable`), then turns on
    /// `mcp.allow_session_summaries` — off by default, and there's no
    /// non-interactive CLI flag to set it directly.
    ///
    /// `suv enable` already serializes a full `[mcp]` table (config.mcp is
    /// a required, not `Option`, field), so it flips the existing
    /// `allow_session_summaries = false` line in place rather than
    /// appending a second `[mcp]` header — appending would produce a
    /// duplicate table, which the `toml` crate rejects as a parse error.
    fn enable_session_summaries(&self) {
        let result = self.run(&["enable"]);
        assert!(result.status.success());
        let config_path = find_file_named(self.home.path(), "config.toml")
            .expect("config.toml should exist after `suv enable`");
        let content = std::fs::read_to_string(&config_path).unwrap();
        assert!(
            content.contains("allow_session_summaries = false"),
            "expected `suv enable` to have written the default \
             `allow_session_summaries = false` line to flip; config.toml was:\n{content}"
        );
        let content = content.replace(
            "allow_session_summaries = false",
            "allow_session_summaries = true",
        );
        std::fs::write(&config_path, content).unwrap();
    }
}

/// Recursively find the first file named exactly `name` under `path`.
/// Walks the whole sandbox home rather than a hardcoded subpath since the
/// actual config/data dir is platform-specific (`directories`'
/// `ProjectDirs`, not `XDG_CONFIG_HOME` on macOS).
fn find_file_named(path: &std::path::Path, name: &str) -> Option<std::path::PathBuf> {
    for entry in std::fs::read_dir(path).unwrap().flatten() {
        let entry_path = entry.path();
        if entry.file_type().unwrap().is_dir() {
            if let Some(found) = find_file_named(&entry_path, name) {
                return Some(found);
            }
        } else if entry_path.file_name().and_then(|n| n.to_str()) == Some(name) {
            return Some(entry_path);
        }
    }
    None
}

fn skill_md_path(s: &Sandbox) -> std::path::PathBuf {
    s.home
        .path()
        .join(".claude/skills/suvadu-session-memory/SKILL.md")
}

#[test]
fn suv_init_claude_code_installs_the_session_memory_skill_when_summaries_enabled() {
    let s = Sandbox::new();
    s.enable_session_summaries();

    let result = s.run(&["init", "claude-code"]);
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );

    let content = std::fs::read_to_string(skill_md_path(&s))
        .expect("SKILL.md should exist immediately after init, before any manual sync");
    assert!(content.starts_with("---\nname: suvadu-session-memory\n"));
    assert!(content.contains("resolve_current_agent_session"));
    assert!(content.contains("save_session_summary"));
}

#[test]
fn suv_init_claude_code_skips_the_skill_when_summaries_disabled() {
    let s = Sandbox::new();
    // mcp.allow_session_summaries is off by default; no config.toml at all.

    let result = s.run(&["init", "claude-code"]);
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );

    assert!(!skill_md_path(&s).exists());
}
