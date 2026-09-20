//! End-to-end acceptance test for PROD-12: a shared skill stays
//! understandable and reversible through its whole life —
//! add → preview → apply → edit → disable/remove → cleanup — across every
//! supported target format (Claude Code `SKILL.md`, Cursor `.mdc`, Codex
//! `AGENTS.md` block).
//!
//! Runs the real `suv` binary against a private `$HOME`, `$CODEX_HOME`,
//! config, and database (same sandbox approach as
//! `session_memory_skill.rs`), so nothing here can touch the developer's
//! own skills, agent config files, or history.
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

struct Sandbox {
    home: tempfile::TempDir,
    project: tempfile::TempDir,
    codex_home: tempfile::TempDir,
}

impl Sandbox {
    fn new() -> Self {
        Self {
            home: tempfile::tempdir().unwrap(),
            project: tempfile::tempdir().unwrap(),
            codex_home: tempfile::tempdir().unwrap(),
        }
    }

    /// A `suv` invocation with a fully sandboxed home, config, and database.
    fn command(&self, dir: &Path, args: &[&str]) -> Command {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_suv"));
        cmd.current_dir(dir)
            .env("HOME", self.home.path())
            .env("CODEX_HOME", self.codex_home.path())
            .env("XDG_DATA_HOME", self.home.path().join("data"))
            .env("XDG_CONFIG_HOME", self.home.path().join("config"))
            .env_remove("SUVADU_PAUSED")
            .env("NO_COLOR", "1")
            .args(args);
        cmd
    }

    fn run_in(&self, dir: &Path, args: &[&str]) -> Output {
        let out = self.command(dir, args).output().unwrap();
        assert!(
            out.status.success(),
            "`suv {}` failed: {}{}",
            args.join(" "),
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        out
    }

    /// Run a command that is expected to fail; returns stderr.
    fn run_expecting_failure(&self, args: &[&str]) -> String {
        let out = self.command(self.project.path(), args).output().unwrap();
        assert!(
            !out.status.success(),
            "`suv {}` unexpectedly succeeded: {}",
            args.join(" "),
            String::from_utf8_lossy(&out.stdout)
        );
        String::from_utf8_lossy(&out.stderr).to_string()
    }

    /// Turn on `mcp.allow_skill_proposals`, which is off by default and has
    /// no non-interactive CLI flag. `suv enable` writes a full `[mcp]`
    /// table, so the line can be flipped in place.
    fn allow_skill_proposals(&self) {
        self.suv(&["enable"]);
        let path = find_file_named(self.home.path(), "config.toml")
            .expect("config.toml should exist after `suv enable`");
        let content = std::fs::read_to_string(&path).unwrap();
        assert!(
            content.contains("allow_skill_proposals = false"),
            "{content}"
        );
        std::fs::write(
            &path,
            content.replace(
                "allow_skill_proposals = false",
                "allow_skill_proposals = true",
            ),
        )
        .unwrap();
    }

    /// Call one MCP tool over `suv mcp-serve`'s line-delimited JSON-RPC and
    /// return the text content of the response.
    fn mcp_call(&self, tool: &str, arguments_json: &str) -> String {
        use std::io::Write as _;
        let request = format!(
            r#"{{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{{"name":"{tool}","arguments":{arguments_json}}}}}"#
        );
        let mut child = self
            .command(self.project.path(), &["mcp-serve"])
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        child
            .stdin
            .as_mut()
            .unwrap()
            .write_all(format!("{request}\n").as_bytes())
            .unwrap();
        drop(child.stdin.take());
        let out = child.wait_with_output().unwrap();
        String::from_utf8_lossy(&out.stdout).to_string()
    }

    /// Run from the project directory — the normal working position.
    fn suv(&self, args: &[&str]) -> String {
        let out = self.run_in(self.project.path(), args);
        String::from_utf8_lossy(&out.stdout).to_string()
    }

    fn claude_skill(&self, name: &str) -> PathBuf {
        self.home
            .path()
            .join(".claude/skills")
            .join(name)
            .join("SKILL.md")
    }

    fn cursor_rule(&self, name: &str) -> PathBuf {
        self.project
            .path()
            .join(".cursor/rules")
            .join(format!("{name}.mdc"))
    }

    fn codex_agents(&self) -> PathBuf {
        self.codex_home.path().join("AGENTS.md")
    }
}

#[test]
#[allow(clippy::too_many_lines)] // one deliberate end-to-end walk of the whole lifecycle
fn a_skill_can_be_added_previewed_applied_edited_disabled_and_cleaned_up() {
    let sb = Sandbox::new();

    // ── add ───────────────────────────────────────────────────────────
    let added = sb.suv(&[
        "skills",
        "add",
        "alpha",
        "--description",
        "the alpha skill",
        "--body",
        "Always run the tests.",
    ]);
    assert!(added.contains("added"), "{added}");

    // The library view answers scope / origin / state without a second step.
    let list = sb.suv(&["skills", "list"]);
    assert!(list.contains("alpha"), "{list}");
    assert!(list.contains("active"), "{list}");
    assert!(list.contains("you"), "origin missing from list:\n{list}");
    assert!(list.contains("global"), "{list}");

    // …and `show` names the exact files it would be written to.
    let shown = sb.suv(&["skills", "show", "alpha"]);
    assert!(shown.contains("syncs to:"), "{shown}");
    assert!(
        shown.contains(&sb.claude_skill("alpha").display().to_string()),
        "{shown}"
    );
    assert!(
        shown.contains(&sb.cursor_rule("alpha").display().to_string()),
        "{shown}"
    );
    assert!(
        shown.contains(&sb.codex_agents().display().to_string()),
        "{shown}"
    );

    // ── preview ───────────────────────────────────────────────────────
    // An existing, hand-written AGENTS.md must survive every step below.
    std::fs::write(sb.codex_agents(), "# House rules\n\nBe concise.\n").unwrap();

    let preview = sb.suv(&["skills", "sync", "--dry-run"]);
    assert!(
        preview.contains(&sb.claude_skill("alpha").display().to_string()),
        "preview must name the exact target file:\n{preview}"
    );
    assert!(
        preview.contains("+ Always run the tests."),
        "preview must show the managed change:\n{preview}"
    );
    assert!(preview.contains("would create"), "{preview}");
    assert!(
        !sb.claude_skill("alpha").exists(),
        "a dry run must not write anything"
    );

    // ── apply ─────────────────────────────────────────────────────────
    let applied = sb.suv(&["skills", "sync"]);
    assert!(applied.contains("file(s) written"), "{applied}");
    for path in [
        sb.claude_skill("alpha"),
        sb.cursor_rule("alpha"),
        sb.codex_agents(),
    ] {
        let content = std::fs::read_to_string(&path).unwrap();
        assert!(
            content.contains("Always run the tests."),
            "{} missing skill body:\n{content}",
            path.display()
        );
    }
    let agents = std::fs::read_to_string(sb.codex_agents()).unwrap();
    assert!(
        agents.contains("Be concise."),
        "unrelated instructions must survive:\n{agents}"
    );

    // Repeat sync is a no-op.
    let again = sb.suv(&["skills", "sync"]);
    assert!(
        again.contains("Nothing to sync"),
        "repeat sync must be idempotent:\n{again}"
    );

    // ── edit ──────────────────────────────────────────────────────────
    sb.suv(&[
        "skills",
        "edit",
        "alpha",
        "--body",
        "Always run the tests twice.",
    ]);
    let edit_preview = sb.suv(&["skills", "sync", "--dry-run"]);
    assert!(
        edit_preview.contains("- Always run the tests."),
        "preview must show what goes away:\n{edit_preview}"
    );
    assert!(
        edit_preview.contains("+ Always run the tests twice."),
        "preview must show what arrives:\n{edit_preview}"
    );
    sb.suv(&["skills", "sync"]);
    assert!(std::fs::read_to_string(sb.claude_skill("alpha"))
        .unwrap()
        .contains("twice"));

    // ── disable (reversible) ──────────────────────────────────────────
    let disabled = sb.suv(&["skills", "disable", "alpha"]);
    assert!(disabled.contains("cleanup"), "{disabled}");
    assert!(
        sb.claude_skill("alpha").exists(),
        "disabling must not delete generated files behind the user's back"
    );
    let list_all = sb.suv(&["skills", "list", "--all"]);
    assert!(list_all.contains("disabled"), "{list_all}");

    let cleanup_preview = sb.suv(&["skills", "cleanup", "--dry-run"]);
    assert!(
        cleanup_preview.contains("would remove"),
        "{cleanup_preview}"
    );
    assert!(
        sb.claude_skill("alpha").exists(),
        "a cleanup dry run must not remove anything"
    );

    // Re-enabling puts everything back — the whole point of `disable`.
    sb.suv(&["skills", "enable", "alpha"]);
    let after_enable = sb.suv(&["skills", "cleanup", "--dry-run"]);
    assert!(
        after_enable.contains("Nothing to clean up"),
        "{after_enable}"
    );

    // ── remove ────────────────────────────────────────────────────────
    let removed = sb.suv(&["skills", "rm", "alpha"]);
    assert!(
        removed.contains("suv skills cleanup"),
        "removal must explain the leftover files:\n{removed}"
    );
    assert!(
        removed.contains(&sb.claude_skill("alpha").display().to_string()),
        "removal must list what is left behind:\n{removed}"
    );
    assert!(
        sb.claude_skill("alpha").exists(),
        "rm must not silently delete generated files"
    );

    // ── cleanup ───────────────────────────────────────────────────────
    let cleaned = sb.suv(&["skills", "cleanup"]);
    assert!(cleaned.contains("removed"), "{cleaned}");
    assert!(!sb.claude_skill("alpha").exists());
    assert!(!sb.cursor_rule("alpha").exists());
    let agents = std::fs::read_to_string(sb.codex_agents()).unwrap();
    assert!(
        !agents.contains("suvadu:skills:start"),
        "the managed block should be gone:\n{agents}"
    );
    assert_eq!(
        agents, "# House rules\n\nBe concise.\n",
        "hand-written AGENTS.md content must come through untouched"
    );
}

#[test]
fn a_hand_edited_agent_file_is_reported_as_a_conflict_and_is_actionable() {
    let sb = Sandbox::new();
    sb.suv(&[
        "skills",
        "add",
        "beta",
        "--description",
        "the beta skill",
        "--body",
        "Original guidance.",
    ]);
    sb.suv(&["skills", "sync", "--target", "claude-code"]);

    // Someone edits the generated file directly.
    let path = sb.claude_skill("beta");
    let edited = std::fs::read_to_string(&path)
        .unwrap()
        .replace("Original guidance.", "Guidance I tweaked by hand.");
    std::fs::write(&path, &edited).unwrap();

    let out = sb.suv(&["skills", "sync", "--target", "claude-code"]);
    assert!(out.contains("CONFLICT"), "{out}");
    assert!(
        out.contains("--force"),
        "must say how to resolve it:\n{out}"
    );
    assert_eq!(
        std::fs::read_to_string(&path).unwrap(),
        edited,
        "a conflicted file must not be overwritten"
    );

    // Cleanup keeps it too — suvadu never destroys an edit it didn't make.
    sb.suv(&["skills", "rm", "beta"]);
    let cleaned = sb.suv(&["skills", "cleanup", "--target", "claude-code"]);
    assert!(cleaned.contains("left alone"), "{cleaned}");
    assert!(path.exists(), "a hand-edited file survives cleanup");

    // --force is the deliberate way through.
    sb.suv(&[
        "skills",
        "add",
        "beta",
        "--description",
        "the beta skill",
        "--body",
        "Original guidance.",
    ]);
    let forced = sb.suv(&["skills", "sync", "--target", "claude-code", "--force"]);
    assert!(forced.contains("file(s) written"), "{forced}");
    assert!(std::fs::read_to_string(&path)
        .unwrap()
        .contains("Original guidance."));
}

#[test]
fn a_project_scoped_skill_wins_over_a_global_one_with_the_same_name() {
    let sb = Sandbox::new();
    sb.suv(&[
        "skills",
        "add",
        "shared",
        "--description",
        "global version",
        "--body",
        "GLOBAL BODY",
    ]);
    sb.suv(&[
        "skills",
        "add",
        "shared",
        "--scope",
        "here",
        "--description",
        "project version",
        "--body",
        "PROJECT BODY",
    ]);

    let out = sb.suv(&["skills", "sync", "--target", "cursor"]);
    assert!(
        out.contains("shadows"),
        "precedence must be stated, not silent:\n{out}"
    );
    let rule = std::fs::read_to_string(sb.cursor_rule("shared")).unwrap();
    assert!(rule.contains("PROJECT BODY"), "{rule}");
    assert!(!rule.contains("GLOBAL BODY"), "{rule}");
}

#[test]
fn an_agent_proposal_never_becomes_active_on_its_own() {
    let sb = Sandbox::new();
    sb.allow_skill_proposals();

    let response = sb.mcp_call(
        "propose_skill",
        r#"{"name":"gated","description":"an agent idea","body":"SHOULD NOT APPEAR","source_agent":"claude-code"}"#,
    );
    assert!(
        response.contains("pending review"),
        "the tool must say the proposal is not live yet:\n{response}"
    );

    // It is visible, labelled, and attributed — but not live.
    let list = sb.suv(&["skills", "list", "--all"]);
    assert!(list.contains("gated"), "{list}");
    assert!(list.contains("pending review"), "{list}");
    assert!(
        list.contains("claude-code"),
        "origin must be shown:\n{list}"
    );
    assert!(
        !sb.suv(&["skills", "list"]).contains("gated"),
        "a proposal must not show up among active skills"
    );

    let shown = sb.suv(&["skills", "show", "gated"]);
    assert!(shown.contains("not synced"), "{shown}");
    assert!(shown.contains("claude-code"), "{shown}");

    // Sync ignores it entirely, and leaves it pending.
    let out = sb.suv(&["skills", "sync"]);
    assert!(out.contains("Nothing to sync"), "{out}");
    assert!(!sb.claude_skill("gated").exists());
    assert!(!sb.cursor_rule("gated").exists());
    let agents = std::fs::read_to_string(sb.codex_agents()).unwrap_or_default();
    assert!(!agents.contains("SHOULD NOT APPEAR"), "{agents}");

    // And `enable` is not a side door around the review queue.
    let refused = sb.run_expecting_failure(&["skills", "enable", "gated"]);
    assert!(refused.contains("review"), "{refused}");
    assert!(sb
        .suv(&["skills", "list", "--all"])
        .contains("pending review"));
}

/// Recursively find the first file named exactly `name` under `path`. The
/// real config directory is platform-specific (`directories`' `ProjectDirs`,
/// not `XDG_CONFIG_HOME` on macOS), so search rather than hardcode.
fn find_file_named(path: &Path, name: &str) -> Option<PathBuf> {
    for entry in std::fs::read_dir(path).ok()?.flatten() {
        let entry_path = entry.path();
        if entry.file_type().ok()?.is_dir() {
            if let Some(found) = find_file_named(&entry_path, name) {
                return Some(found);
            }
        } else if entry_path.file_name().and_then(|n| n.to_str()) == Some(name) {
            return Some(entry_path);
        }
    }
    None
}
