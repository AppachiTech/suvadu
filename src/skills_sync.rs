//! Materializes active skills into each AI agent's own native file format.
//!
//! Suvadu's `skills` table stays the source of truth; each agent's file is a
//! generated artifact. This exists because MCP tools are *pull* — an agent
//! only sees `list_skills`/`get_skill` if it decides to call them — so a host
//! that doesn't (yet) read skills over MCP still benefits once `suv skills
//! sync` has run. Every write here only touches content this module itself
//! owns (a whole per-skill file for Claude Code/Cursor, or a clearly marked
//! block for Codex's `AGENTS.md`) so hand-written content next to it survives
//! untouched, and a no-op sync never rewrites a file.

use crate::cli::SyncTarget;
use crate::models::{Skill, SKILL_SCOPE_GLOBAL, SKILL_STATUS_ACTIVE};
use crate::repository::Repository;
use crate::util::atomic_write_with_mode;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

const CODEX_BLOCK_START: &str = "<!-- suvadu:skills:start -->";
const CODEX_BLOCK_END: &str = "<!-- suvadu:skills:end -->";

pub struct SyncReport {
    pub written: usize,
    pub lines: Vec<String>,
}

impl SyncReport {
    fn note(&mut self, line: String) {
        self.lines.push(line);
    }
}

/// Sync all `active` skills that are either global or scoped to `cwd` into
/// each requested `target`'s native format. Resolves `HOME`/`CODEX_HOME`
/// once here (a read, done by every real caller) and hands them down as
/// plain parameters so the rest of this module — and its tests — never
/// touch process environment.
pub fn sync(
    repo: &Repository,
    targets: &[SyncTarget],
    cwd: &Path,
    dry_run: bool,
) -> Result<SyncReport, Box<dyn std::error::Error>> {
    let home = std::env::var_os("HOME").map(PathBuf::from);
    let codex_home_override = std::env::var_os("CODEX_HOME")
        .filter(|p| !p.is_empty())
        .map(PathBuf::from);
    sync_inner(
        repo,
        targets,
        cwd,
        home.as_deref(),
        codex_home_override.as_deref(),
        dry_run,
    )
}

/// Testable core of [`sync`] — takes already-resolved paths instead of
/// reading the environment, so tests can supply temp directories directly.
fn sync_inner(
    repo: &Repository,
    targets: &[SyncTarget],
    cwd: &Path,
    home: Option<&Path>,
    codex_home_override: Option<&Path>,
    dry_run: bool,
) -> Result<SyncReport, Box<dyn std::error::Error>> {
    let all = repo.list_skills(None, Some(SKILL_STATUS_ACTIVE))?;
    let cwd_str = cwd.to_string_lossy().to_string();
    let relevant: Vec<&Skill> = all
        .iter()
        .filter(|s| s.scope == SKILL_SCOPE_GLOBAL || s.scope == cwd_str)
        .collect();

    let mut report = SyncReport {
        written: 0,
        lines: Vec::new(),
    };
    if relevant.is_empty() {
        return Ok(report);
    }

    for target in targets {
        match target {
            SyncTarget::ClaudeCode => sync_claude_code(&relevant, home, cwd, dry_run, &mut report)?,
            SyncTarget::Cursor => sync_cursor(&relevant, cwd, dry_run, &mut report)?,
            SyncTarget::Codex => {
                sync_codex(
                    &relevant,
                    home,
                    codex_home_override,
                    cwd,
                    dry_run,
                    &mut report,
                )?;
            }
        }
    }

    Ok(report)
}

/// Write `content` to `path` only if it differs from what's already there.
/// Creates parent directories as needed. Returns `true` if a write happened
/// (or would have, under `dry_run`).
fn write_if_changed(
    path: &Path,
    content: &str,
    dry_run: bool,
    report: &mut SyncReport,
) -> Result<bool, Box<dyn std::error::Error>> {
    let existing = std::fs::read_to_string(path).ok();
    if existing.as_deref() == Some(content) {
        report.note(format!("  unchanged  {}", path.display()));
        return Ok(false);
    }
    if dry_run {
        report.note(format!("  would write  {}", path.display()));
        return Ok(true);
    }
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    atomic_write_with_mode(path, content, 0o600)?;
    report.note(format!("  wrote  {}", path.display()));
    Ok(true)
}

fn skill_target_dir(skill: &Skill, global_root: &Path, project_root: &Path) -> PathBuf {
    if skill.scope == SKILL_SCOPE_GLOBAL {
        global_root.to_path_buf()
    } else {
        project_root.to_path_buf()
    }
}

// ── Claude Code: one directory per skill, `SKILL.md` with frontmatter ──────

fn sync_claude_code(
    skills: &[&Skill],
    home: Option<&Path>,
    cwd: &Path,
    dry_run: bool,
    report: &mut SyncReport,
) -> Result<(), Box<dyn std::error::Error>> {
    let Some(home) = home else {
        report.note("  skipped claude-code (HOME not set)".to_string());
        return Ok(());
    };
    let global_root = home.join(".claude").join("skills");
    let project_root = cwd.join(".claude").join("skills");

    report.note("Claude Code:".to_string());
    for skill in skills {
        let dir = skill_target_dir(skill, &global_root, &project_root).join(&skill.name);
        let path = dir.join("SKILL.md");
        let content = format!(
            "---\nname: {}\ndescription: {}\n---\n\n{}\n",
            skill.name,
            escape_yaml_scalar(&skill.description),
            skill.body.trim_end()
        );
        if write_if_changed(&path, &content, dry_run, report)? {
            report.written += 1;
        }
    }
    Ok(())
}

// ── Cursor: one `.mdc` rule file per skill (project-scoped only) ───────────

fn sync_cursor(
    skills: &[&Skill],
    cwd: &Path,
    dry_run: bool,
    report: &mut SyncReport,
) -> Result<(), Box<dyn std::error::Error>> {
    // Cursor has no stable global rules location, so both global and
    // project-scoped skills land in this project's .cursor/rules/.
    let rules_dir = cwd.join(".cursor").join("rules");

    report.note("Cursor (project rules — no global location to sync into):".to_string());
    for skill in skills {
        let path = rules_dir.join(format!("{}.mdc", skill.name));
        let content = format!(
            "---\ndescription: {}\nalwaysApply: false\n---\n\n{}\n",
            escape_yaml_scalar(&skill.description),
            skill.body.trim_end()
        );
        if write_if_changed(&path, &content, dry_run, report)? {
            report.written += 1;
        }
    }
    Ok(())
}

// ── Codex: one managed block inside AGENTS.md per scope ────────────────────

fn sync_codex(
    skills: &[&Skill],
    home: Option<&Path>,
    codex_home_override: Option<&Path>,
    cwd: &Path,
    dry_run: bool,
    report: &mut SyncReport,
) -> Result<(), Box<dyn std::error::Error>> {
    report.note("Codex (AGENTS.md):".to_string());
    let global: Vec<&&Skill> = skills
        .iter()
        .filter(|s| s.scope == SKILL_SCOPE_GLOBAL)
        .collect();
    let project: Vec<&&Skill> = skills
        .iter()
        .filter(|s| s.scope != SKILL_SCOPE_GLOBAL)
        .collect();

    if !global.is_empty() {
        let codex_home = codex_home_override
            .map(Path::to_path_buf)
            .or_else(|| home.map(|h| h.join(".codex")));
        if let Some(codex_home) = codex_home {
            sync_codex_file(&codex_home.join("AGENTS.md"), &global, dry_run, report)?;
        } else {
            report.note("  skipped global (HOME/CODEX_HOME not set)".to_string());
        }
    }
    if !project.is_empty() {
        sync_codex_file(&cwd.join("AGENTS.md"), &project, dry_run, report)?;
    }
    Ok(())
}

fn sync_codex_file(
    path: &Path,
    skills: &[&&Skill],
    dry_run: bool,
    report: &mut SyncReport,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut block = String::new();
    block.push_str(CODEX_BLOCK_START);
    block.push_str("\n<!-- Managed by suvadu — edit via `suv skills edit`, then `suv skills sync`, not by hand. -->\n");
    for skill in skills {
        let _ = write!(block, "\n## {}\n\n", skill.name);
        if !skill.description.is_empty() {
            let _ = writeln!(block, "{}\n", skill.description);
        }
        block.push_str(skill.body.trim_end());
        block.push('\n');
    }
    block.push_str(CODEX_BLOCK_END);

    let existing = std::fs::read_to_string(path).unwrap_or_default();
    let updated =
        replace_or_append_managed_block(&existing, CODEX_BLOCK_START, CODEX_BLOCK_END, &block);

    if updated == existing {
        report.note(format!("  unchanged  {}", path.display()));
        return Ok(());
    }
    if dry_run {
        report.note(format!("  would write  {}", path.display()));
        report.written += 1;
        return Ok(());
    }
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    atomic_write_with_mode(path, &updated, 0o600)?;
    report.note(format!("  wrote  {}", path.display()));
    report.written += 1;
    Ok(())
}

/// Replace the span between `start_marker`/`end_marker` with `new_block`
/// (which itself includes the markers), or append `new_block` if the markers
/// aren't present yet. Everything outside the markers is left untouched.
fn replace_or_append_managed_block(
    existing: &str,
    start_marker: &str,
    end_marker: &str,
    new_block: &str,
) -> String {
    if let Some(start_idx) = existing.find(start_marker) {
        if let Some(end_rel) = existing[start_idx..].find(end_marker) {
            let end_idx = start_idx + end_rel + end_marker.len();
            let mut out = String::new();
            out.push_str(&existing[..start_idx]);
            out.push_str(new_block);
            out.push_str(&existing[end_idx..]);
            return out;
        }
    }
    let mut out = existing.to_string();
    if !out.is_empty() {
        if !out.ends_with('\n') {
            out.push('\n');
        }
        out.push('\n');
    }
    out.push_str(new_block);
    out.push('\n');
    out
}

/// Minimal YAML scalar escaping for a single-line frontmatter value: wrap in
/// double quotes and escape embedded quotes/backslashes/newlines.
fn escape_yaml_scalar(s: &str) -> String {
    let escaped = s
        .replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('\n', " ");
    format!("\"{escaped}\"")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::NewSkill;
    use crate::test_utils::test_repo;

    fn skill(name: &str, scope: &str) -> NewSkill {
        NewSkill {
            name: name.to_string(),
            description: format!("{name} desc"),
            body: format!("Body for {name}."),
            triggers: vec![],
            scope: scope.to_string(),
            source: "human".to_string(),
            status: SKILL_STATUS_ACTIVE.to_string(),
        }
    }

    #[test]
    fn sync_claude_code_writes_skill_md_with_frontmatter() {
        let (_dir, repo) = test_repo();
        repo.create_skill(&skill("deploy", SKILL_SCOPE_GLOBAL))
            .unwrap();

        let home = tempfile::tempdir().unwrap();
        let cwd = tempfile::tempdir().unwrap();
        let report = sync_inner(
            &repo,
            &[SyncTarget::ClaudeCode],
            cwd.path(),
            Some(home.path()),
            None,
            false,
        )
        .unwrap();

        let content =
            std::fs::read_to_string(home.path().join(".claude/skills/deploy/SKILL.md")).unwrap();
        assert!(content.starts_with("---\nname: deploy\n"));
        assert!(content.contains("Body for deploy."));
        assert_eq!(report.written, 1);
    }

    #[test]
    fn sync_is_idempotent_second_run_writes_nothing() {
        let (_dir, repo) = test_repo();
        repo.create_skill(&skill("deploy", SKILL_SCOPE_GLOBAL))
            .unwrap();
        let home = tempfile::tempdir().unwrap();
        let cwd = tempfile::tempdir().unwrap();

        let targets = [SyncTarget::ClaudeCode];
        let first =
            sync_inner(&repo, &targets, cwd.path(), Some(home.path()), None, false).unwrap();
        assert_eq!(first.written, 1);

        let second =
            sync_inner(&repo, &targets, cwd.path(), Some(home.path()), None, false).unwrap();
        assert_eq!(
            second.written, 0,
            "unchanged content should not be rewritten"
        );
    }

    #[test]
    fn sync_project_scoped_skill_only_written_for_matching_cwd() {
        let (_dir, repo) = test_repo();
        let project_dir = tempfile::tempdir().unwrap();
        repo.create_skill(&skill("local-fix", &project_dir.path().to_string_lossy()))
            .unwrap();
        let home = tempfile::tempdir().unwrap();

        // A sync from an unrelated directory should not pick up the
        // project-scoped skill.
        let other_dir = tempfile::tempdir().unwrap();
        let report = sync_inner(
            &repo,
            &[SyncTarget::ClaudeCode],
            other_dir.path(),
            Some(home.path()),
            None,
            false,
        )
        .unwrap();
        assert_eq!(report.written, 0);

        // Syncing from the matching project directory does.
        let report = sync_inner(
            &repo,
            &[SyncTarget::ClaudeCode],
            project_dir.path(),
            Some(home.path()),
            None,
            false,
        )
        .unwrap();
        assert_eq!(report.written, 1);
    }

    #[test]
    fn dry_run_does_not_write_files() {
        let (_dir, repo) = test_repo();
        repo.create_skill(&skill("preview-me", SKILL_SCOPE_GLOBAL))
            .unwrap();
        let home = tempfile::tempdir().unwrap();
        let cwd = tempfile::tempdir().unwrap();

        let report = sync_inner(
            &repo,
            &[SyncTarget::ClaudeCode],
            cwd.path(),
            Some(home.path()),
            None,
            true,
        )
        .unwrap();
        assert_eq!(report.written, 1);
        assert!(!home
            .path()
            .join(".claude/skills/preview-me/SKILL.md")
            .exists());
    }

    #[test]
    fn sync_codex_preserves_hand_written_content_around_managed_block() {
        let (_dir, repo) = test_repo();
        repo.create_skill(&skill("ci-tips", SKILL_SCOPE_GLOBAL))
            .unwrap();
        let cwd = tempfile::tempdir().unwrap();
        let codex_home = tempfile::tempdir().unwrap();
        std::fs::write(
            codex_home.path().join("AGENTS.md"),
            "# My hand-written notes\n\nDon't touch this.\n",
        )
        .unwrap();

        sync_inner(
            &repo,
            &[SyncTarget::Codex],
            cwd.path(),
            None,
            Some(codex_home.path()),
            false,
        )
        .unwrap();
        let content = std::fs::read_to_string(codex_home.path().join("AGENTS.md")).unwrap();
        assert!(content.contains("Don't touch this."));
        assert!(content.contains(CODEX_BLOCK_START));
        assert!(content.contains("ci-tips"));

        // Re-running should replace only the managed block, not duplicate it.
        sync_inner(
            &repo,
            &[SyncTarget::Codex],
            cwd.path(),
            None,
            Some(codex_home.path()),
            false,
        )
        .unwrap();
        let content2 = std::fs::read_to_string(codex_home.path().join("AGENTS.md")).unwrap();
        assert_eq!(content2.matches(CODEX_BLOCK_START).count(), 1);
        assert_eq!(content, content2);
    }

    #[test]
    fn replace_or_append_managed_block_appends_when_absent() {
        let out = replace_or_append_managed_block(
            "existing content\n",
            "<start>",
            "<end>",
            "<start>new<end>",
        );
        assert!(out.starts_with("existing content\n"));
        assert!(out.contains("<start>new<end>"));
    }

    #[test]
    fn replace_or_append_managed_block_replaces_existing_span() {
        let existing = "before\n<start>old<end>\nafter\n";
        let out = replace_or_append_managed_block(existing, "<start>", "<end>", "<start>new<end>");
        assert_eq!(out, "before\n<start>new<end>\nafter\n");
    }
}
