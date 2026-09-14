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
    /// Same environment as `command()`, but with `current_dir` overridden —
    /// for exercising behavior that depends on the process cwd differing
    /// from `$HOME`, while keeping the same sandboxed database/config.
    fn command_in(&self, dir: &std::path::Path) -> Command {
        let mut cmd = self.command();
        cmd.current_dir(dir);
        cmd
    }
    fn run_in(&self, dir: &std::path::Path, args: &[&str]) -> Output {
        self.command_in(dir).args(args).output().unwrap()
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

#[test]
fn suv_skills_sync_installs_the_skill_without_a_prior_init() {
    let s = Sandbox::new();
    s.enable_session_summaries();

    // No `suv init claude-code` at all — just a bare sync.
    let result = s.run(&["skills", "sync"]);
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );

    let content = std::fs::read_to_string(skill_md_path(&s))
        .expect("suv skills sync alone should install and materialize the skill");
    assert!(content.contains("resolve_current_agent_session"));
}

#[test]
fn suv_skills_sync_default_reaches_cursor_and_codex_too() {
    let s = Sandbox::new();
    s.enable_session_summaries();

    let result = s.run(&["skills", "sync"]);
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );

    // Cursor has no stable global rules location, so a global skill lands
    // in the *current directory's* .cursor/rules/ — `Sandbox::command()`
    // sets `current_dir` to the sandbox's own tempdir specifically so this
    // (and nothing else) is where `suv skills sync`'s child process sees
    // as its cwd, rather than polluting the real cargo test run's cwd.
    let cursor_rule = s
        .home
        .path()
        .join(".cursor/rules/suvadu-session-memory.mdc");
    assert!(
        cursor_rule.exists(),
        "expected {} to exist",
        cursor_rule.display()
    );

    let codex_agents = find_file_named(s.home.path(), "AGENTS.md")
        .expect("Codex's AGENTS.md should exist after a default sync");
    let agents_content = std::fs::read_to_string(&codex_agents).unwrap();
    assert!(agents_content.contains("resolve_current_agent_session"));
}

#[test]
fn suv_skills_sync_dry_run_does_not_persist_the_builtin_skill_seed() {
    let s = Sandbox::new();
    s.enable_session_summaries();

    // No prior `suv init claude-code` or `suv skills sync` — the skill
    // has never been seeded into the DB before this dry run.
    let result = s.run(&["skills", "sync", "--dry-run"]);
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );

    // `--dry-run` must preview without writing — including to the
    // database. `ensure_installed` upserts a row, so it must be skipped
    // entirely on a dry run rather than merely suppressing the file
    // writes it feeds into.
    let list = s.run(&["skills", "list", "--json"]);
    assert!(
        list.status.success(),
        "{}",
        String::from_utf8_lossy(&list.stderr)
    );
    let list_json = String::from_utf8_lossy(&list.stdout);
    assert!(
        !list_json.contains("suvadu-session-memory"),
        "suv skills sync --dry-run must not persist the builtin skill into \
         the skills table, but `suv skills list --json` shows it:\n{list_json}"
    );

    // No files should have been written either.
    assert!(!skill_md_path(&s).exists());
}

#[test]
fn suv_init_claude_code_still_materializes_after_a_prior_sync_already_seeded_it() {
    let s = Sandbox::new();
    s.enable_session_summaries();

    // Seed the skill via a bare sync first (no prior init) — after this,
    // the DB row is already at its current version, so `ensure_installed`
    // reports `changed = false` on every subsequent call.
    let sync_result = s.run(&["skills", "sync"]);
    assert!(
        sync_result.status.success(),
        "{}",
        String::from_utf8_lossy(&sync_result.stderr)
    );
    let skill_md = skill_md_path(&s);
    assert!(skill_md.exists(), "sync should have materialized SKILL.md");

    // Simulate the materialized file going missing (e.g. a prior transient
    // sync failure, or a user deleting it) while the DB row stays seeded.
    // Before this fix, `try_install_builtin_skills` only ran the sync when
    // `ensure_installed` reported a change, so this file would stay
    // missing forever even after a fresh `suv init claude-code`.
    std::fs::remove_file(&skill_md).unwrap();
    assert!(!skill_md.exists());

    let init_result = s.run(&["init", "claude-code"]);
    assert!(
        init_result.status.success(),
        "{}",
        String::from_utf8_lossy(&init_result.stderr)
    );
    let stdout = String::from_utf8_lossy(&init_result.stdout);
    assert!(
        stdout.contains("Installed the suvadu-session-memory skill"),
        "expected the checkmark line to still print even though the skill \
         row was already seeded by the prior sync; stdout was:\n{stdout}"
    );

    let content = std::fs::read_to_string(&skill_md).expect(
        "suv init claude-code should re-materialize SKILL.md even when \
         ensure_installed reports no change",
    );
    assert!(content.contains("resolve_current_agent_session"));
}

#[test]
fn suv_init_claude_code_syncs_against_home_not_the_process_cwd() {
    let s = Sandbox::new();
    s.enable_session_summaries();

    // A project directory that is NOT under $HOME, holding its own
    // project-scoped skill (scope == this directory's path).
    let project_dir = tempfile::tempdir().unwrap();
    let add_result = s.run_in(
        project_dir.path(),
        &[
            "skills",
            "add",
            "project-only-skill",
            "--description",
            "d",
            "--body",
            "b",
            "--scope",
            "here",
        ],
    );
    assert!(
        add_result.status.success(),
        "{}",
        String::from_utf8_lossy(&add_result.stderr)
    );

    // Run `suv init claude-code` FROM that project directory.
    let init_result = s.run_in(project_dir.path(), &["init", "claude-code"]);
    assert!(
        init_result.status.success(),
        "{}",
        String::from_utf8_lossy(&init_result.stderr)
    );

    // The global builtin skill still lands under $HOME.
    assert!(skill_md_path(&s).exists());

    // But nothing should have been written under the project directory.
    // Before this fix, `try_install_builtin_skills` synced against
    // `std::env::current_dir()`, which picked up this project-scoped
    // skill (`scope == cwd_str`) and wrote it into the project
    // directory — something `suv init claude-code` had never done
    // before this feature.
    assert!(
        !project_dir.path().join(".claude").exists(),
        "suv init claude-code must not write anything under the process cwd"
    );
}
