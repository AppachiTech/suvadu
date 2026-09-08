//! `suv skills` — CRUD, review, and native-format sync for the shared
//! cross-agent skills library (see `repository/skills.rs` for storage and
//! `skills_sync.rs` for materialization into each agent's own file format).

use crate::cli::SkillsCommands;
use crate::db;
use crate::models::{
    NewSkill, Skill, SKILL_SCOPE_GLOBAL, SKILL_SOURCE_HUMAN, SKILL_STATUS_ACTIVE,
    SKILL_STATUS_ARCHIVED, SKILL_STATUS_PENDING,
};
use crate::repository::Repository;
use crate::util;
use std::io::Read;

pub fn handle_skills(cmd: Option<SkillsCommands>) -> Result<(), Box<dyn std::error::Error>> {
    let repo = Repository::init()?;
    match cmd {
        Some(cmd) => handle_skills_with_repo(&repo, cmd),
        None => crate::skills_ui::run(&repo),
    }
}

/// "global" (default), "here" (resolved to the current directory), or a
/// literal directory path passed through as-is.
fn resolve_scope(scope_arg: Option<&str>) -> Result<String, Box<dyn std::error::Error>> {
    match scope_arg {
        None | Some(SKILL_SCOPE_GLOBAL) => Ok(SKILL_SCOPE_GLOBAL.to_string()),
        Some("here") => Ok(std::env::current_dir()?.to_string_lossy().to_string()),
        Some(other) => Ok(other.to_string()),
    }
}

fn read_stdin_to_string() -> std::io::Result<String> {
    let mut buf = String::new();
    std::io::stdin().read_to_string(&mut buf)?;
    Ok(buf)
}

fn handle_skills_with_repo(
    repo: &Repository,
    cmd: SkillsCommands,
) -> Result<(), Box<dyn std::error::Error>> {
    match cmd {
        SkillsCommands::Add {
            name,
            description,
            body,
            scope,
            triggers,
        } => handle_add(repo, &name, description, body, scope.as_deref(), triggers),
        SkillsCommands::List { scope, all, json } => handle_list(repo, scope.as_deref(), all, json),
        SkillsCommands::Show { name, scope } => handle_show(repo, &name, scope.as_deref()),
        SkillsCommands::Edit {
            name,
            scope,
            description,
            body,
            triggers,
        } => handle_edit(
            repo,
            &name,
            scope.as_deref(),
            description.as_deref(),
            body.as_deref(),
            &triggers,
        ),
        SkillsCommands::Rm { name, scope } => handle_rm(repo, &name, scope.as_deref()),
        SkillsCommands::Sync { target, dry_run } => handle_sync(repo, target, dry_run),
    }
}

fn handle_add(
    repo: &Repository,
    name: &str,
    description: Option<String>,
    body: Option<String>,
    scope: Option<&str>,
    triggers: Vec<String>,
) -> Result<(), Box<dyn std::error::Error>> {
    if name.trim().is_empty() {
        return Err("skill name cannot be empty".into());
    }
    let scope = resolve_scope(scope)?;
    let body = match body {
        Some(b) => b,
        None => read_stdin_to_string()?,
    };
    if body.trim().is_empty() {
        return Err(
            "skill body is empty — pass --body \"...\" or pipe content via stdin (e.g. `suv skills add foo < notes.md`)"
                .into(),
        );
    }

    let new = NewSkill {
        name: name.to_string(),
        description: description.unwrap_or_default(),
        body,
        triggers,
        scope: scope.clone(),
        source: SKILL_SOURCE_HUMAN.to_string(),
        status: SKILL_STATUS_ACTIVE.to_string(),
    };

    match repo.create_skill(&new) {
        Ok(skill) => {
            println!("✓ Skill '{}' added ({})", skill.name, skill.scope);
            Ok(())
        }
        Err(e) => {
            if let db::DbError::Sqlite(rusqlite::Error::SqliteFailure(err, _)) = &e {
                if err.code == rusqlite::ErrorCode::ConstraintViolation {
                    return Err(format!(
                        "Skill '{name}' already exists in scope '{scope}'. Use `suv skills edit` to change it."
                    )
                    .into());
                }
            }
            Err(e.into())
        }
    }
}

fn handle_list(
    repo: &Repository,
    scope: Option<&str>,
    all: bool,
    json: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    let scope_filter = scope.map(|s| resolve_scope(Some(s))).transpose()?;
    let status_filter = if all { None } else { Some(SKILL_STATUS_ACTIVE) };
    let skills = repo.list_skills(scope_filter.as_deref(), status_filter)?;

    if json {
        println!("{}", serde_json::to_string_pretty(&skills)?);
        return Ok(());
    }

    if skills.is_empty() {
        println!("No skills yet. Use `suv skills add <name>` to save one.");
        return Ok(());
    }

    let max_name = skills
        .iter()
        .map(|s| s.name.len())
        .max()
        .unwrap_or(4)
        .max(4);
    if util::color_enabled() {
        println!(
            "\x1b[1m{:<width$} {:<9} {:<20} Description\x1b[0m",
            "Name",
            "Status",
            "Scope",
            width = max_name
        );
    } else {
        println!(
            "{:<width$} {:<9} {:<20} Description",
            "Name",
            "Status",
            "Scope",
            width = max_name
        );
    }
    for s in &skills {
        let scope_display = if s.scope == SKILL_SCOPE_GLOBAL {
            SKILL_SCOPE_GLOBAL.to_string()
        } else {
            util::truncate_str(&s.scope, 18, "…")
        };
        let desc = util::truncate_str(&s.description, 50, "…");
        println!(
            "{:<width$} {:<9} {:<20} {}",
            s.name,
            s.status,
            scope_display,
            desc,
            width = max_name
        );
    }
    println!("\n{} skill(s)", skills.len());
    Ok(())
}

fn resolve_target(
    repo: &Repository,
    name: &str,
    scope: Option<&str>,
) -> Result<Skill, Box<dyn std::error::Error>> {
    let skill = match scope {
        Some(s) => {
            let scope = resolve_scope(Some(s))?;
            repo.get_skill(name, &scope)?
        }
        None => repo.find_skill(name, None)?,
    };
    skill.ok_or_else(|| {
        format!(
            "No active skill named '{name}' found. Pass --scope to target a specific one, or check `suv skills list --all`."
        )
        .into()
    })
}

fn print_skill(s: &Skill) {
    println!("{}", s.name);
    println!("  scope:   {}", s.scope);
    println!("  status:  {}", s.status);
    println!("  source:  {}", s.source);
    println!("  version: {}", s.version);
    if !s.triggers.is_empty() {
        println!("  triggers: {}", s.triggers.join(", "));
    }
    if !s.description.is_empty() {
        println!("  description: {}", s.description);
    }
    println!("\n{}", s.body);
}

fn handle_show(
    repo: &Repository,
    name: &str,
    scope: Option<&str>,
) -> Result<(), Box<dyn std::error::Error>> {
    let skill = resolve_target(repo, name, scope)?;
    print_skill(&skill);
    Ok(())
}

fn handle_edit(
    repo: &Repository,
    name: &str,
    scope: Option<&str>,
    description: Option<&str>,
    body: Option<&str>,
    triggers: &[String],
) -> Result<(), Box<dyn std::error::Error>> {
    let existing = resolve_target(repo, name, scope)?;
    let body = match body {
        Some("-") => Some(read_stdin_to_string()?),
        other => other.map(str::to_string),
    };
    let triggers_arg = if triggers.is_empty() {
        None
    } else {
        Some(triggers)
    };

    let updated = repo
        .update_skill(
            &existing.name,
            &existing.scope,
            description,
            body.as_deref(),
            triggers_arg,
        )?
        .ok_or("skill disappeared during edit")?;
    println!("✓ Skill '{}' updated (v{})", updated.name, updated.version);
    Ok(())
}

fn handle_rm(
    repo: &Repository,
    name: &str,
    scope: Option<&str>,
) -> Result<(), Box<dyn std::error::Error>> {
    let existing = resolve_target(repo, name, scope)?;
    repo.delete_skill(&existing.name, &existing.scope)?;
    println!("✓ Skill '{}' removed ({})", existing.name, existing.scope);
    Ok(())
}

/// A human's decision on one pending skill proposal.
pub(crate) enum ReviewDecision {
    Approve,
    Reject,
}

/// Apply a review decision to a pending skill. Shared between the TUI's
/// review queue (see `skills_ui.rs`) and this module's tests, so the
/// decision logic is defined once and tested once.
pub(crate) fn apply_review_decision(
    repo: &Repository,
    skill: &Skill,
    decision: &ReviewDecision,
) -> Result<(), Box<dyn std::error::Error>> {
    match decision {
        ReviewDecision::Approve => {
            repo.set_skill_status(&skill.name, &skill.scope, SKILL_STATUS_ACTIVE)?;
        }
        ReviewDecision::Reject => {
            // Kept as archived rather than deleted, so a rejected proposal
            // stays visible in `suv skills list --all` for audit purposes.
            repo.set_skill_status(&skill.name, &skill.scope, SKILL_STATUS_ARCHIVED)?;
        }
    }
    Ok(())
}

fn handle_sync(
    repo: &Repository,
    target: Option<crate::cli::SyncTarget>,
    dry_run: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    use crate::cli::SyncTarget;
    let targets: Vec<SyncTarget> = target.map_or_else(
        || {
            vec![
                SyncTarget::ClaudeCode,
                SyncTarget::Cursor,
                SyncTarget::Codex,
            ]
        },
        |t| vec![t],
    );
    let cwd = std::env::current_dir()?;
    let report = crate::skills_sync::sync(repo, &targets, &cwd, dry_run)?;
    for line in &report.lines {
        println!("{line}");
    }
    if report.written == 0 {
        println!("Nothing to sync — no active skills, or everything already up to date.");
    } else if dry_run {
        println!(
            "\n{} file(s) would be written. Re-run without --dry-run to apply.",
            report.written
        );
    } else {
        println!("\n{} file(s) written.", report.written);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_utils::test_repo;

    fn add(
        repo: &Repository,
        name: &str,
        scope: Option<&str>,
        body: &str,
    ) -> Result<(), Box<dyn std::error::Error>> {
        handle_add(
            repo,
            name,
            Some(format!("{name} desc")),
            Some(body.to_string()),
            scope,
            vec!["trigger-a".into()],
        )
    }

    #[test]
    fn add_and_show_roundtrip() {
        let (_dir, repo) = test_repo();
        add(&repo, "deploy-checklist", None, "# steps").unwrap();

        let skill = resolve_target(&repo, "deploy-checklist", None).unwrap();
        assert_eq!(skill.scope, SKILL_SCOPE_GLOBAL);
        assert_eq!(skill.body, "# steps");
        assert_eq!(skill.status, SKILL_STATUS_ACTIVE);
    }

    #[test]
    fn add_empty_body_errors() {
        let (_dir, repo) = test_repo();
        let result = handle_add(&repo, "empty", None, Some(String::new()), None, vec![]);
        assert!(result.is_err());
    }

    #[test]
    fn add_duplicate_name_scope_errors_with_hint() {
        let (_dir, repo) = test_repo();
        add(&repo, "dup", None, "body").unwrap();
        let result = add(&repo, "dup", None, "body2");
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("suv skills edit"));
    }

    #[test]
    fn same_name_different_scope_allowed() {
        let (_dir, repo) = test_repo();
        add(&repo, "dup", None, "global body").unwrap();
        add(&repo, "dup", Some("/tmp/proj"), "project body").unwrap();
        assert_eq!(repo.list_skills(None, None).unwrap().len(), 2);
    }

    #[test]
    fn edit_updates_body_and_bumps_version() {
        let (_dir, repo) = test_repo();
        add(&repo, "s1", None, "old body").unwrap();
        handle_edit(&repo, "s1", None, None, Some("new body"), &[]).unwrap();

        let skill = resolve_target(&repo, "s1", None).unwrap();
        assert_eq!(skill.body, "new body");
        assert_eq!(skill.version, 2);
    }

    #[test]
    fn rm_removes_skill() {
        let (_dir, repo) = test_repo();
        add(&repo, "temp", None, "body").unwrap();
        handle_rm(&repo, "temp", None).unwrap();
        assert!(resolve_target(&repo, "temp", None).is_err());
    }

    #[test]
    fn rm_missing_skill_errors() {
        let (_dir, repo) = test_repo();
        assert!(handle_rm(&repo, "missing", None).is_err());
    }

    #[test]
    fn list_default_hides_non_active() {
        let (_dir, repo) = test_repo();
        add(&repo, "active-one", None, "body").unwrap();
        repo.create_skill(&NewSkill {
            name: "pending-one".into(),
            description: String::new(),
            body: "body".into(),
            triggers: vec![],
            scope: SKILL_SCOPE_GLOBAL.into(),
            source: "agent:claude-code".into(),
            status: SKILL_STATUS_PENDING.into(),
        })
        .unwrap();

        let active_only = repo.list_skills(None, Some(SKILL_STATUS_ACTIVE)).unwrap();
        assert_eq!(active_only.len(), 1);
        let everything = repo.list_skills(None, None).unwrap();
        assert_eq!(everything.len(), 2);
    }

    #[test]
    fn review_approve_activates_pending_skill() {
        let (_dir, repo) = test_repo();
        let skill = repo
            .create_skill(&NewSkill {
                name: "proposed".into(),
                description: "an agent's idea".into(),
                body: "do X then Y".into(),
                triggers: vec![],
                scope: SKILL_SCOPE_GLOBAL.into(),
                source: "agent:claude-code".into(),
                status: SKILL_STATUS_PENDING.into(),
            })
            .unwrap();

        apply_review_decision(&repo, &skill, &ReviewDecision::Approve).unwrap();
        let updated = repo
            .get_skill("proposed", SKILL_SCOPE_GLOBAL)
            .unwrap()
            .unwrap();
        assert_eq!(updated.status, SKILL_STATUS_ACTIVE);
    }

    #[test]
    fn review_reject_archives_pending_skill() {
        let (_dir, repo) = test_repo();
        let skill = repo
            .create_skill(&NewSkill {
                name: "proposed2".into(),
                description: "an agent's idea".into(),
                body: "do X then Y".into(),
                triggers: vec![],
                scope: SKILL_SCOPE_GLOBAL.into(),
                source: "agent:claude-code".into(),
                status: SKILL_STATUS_PENDING.into(),
            })
            .unwrap();

        apply_review_decision(&repo, &skill, &ReviewDecision::Reject).unwrap();
        let updated = repo
            .get_skill("proposed2", SKILL_SCOPE_GLOBAL)
            .unwrap()
            .unwrap();
        assert_eq!(updated.status, SKILL_STATUS_ARCHIVED);
        // Rejected skills stay out of the default (active-only) list.
        assert!(repo
            .list_skills(None, Some(SKILL_STATUS_ACTIVE))
            .unwrap()
            .is_empty());
    }

    #[test]
    fn resolve_scope_variants() {
        assert_eq!(resolve_scope(None).unwrap(), SKILL_SCOPE_GLOBAL);
        assert_eq!(resolve_scope(Some("global")).unwrap(), SKILL_SCOPE_GLOBAL);
        assert_eq!(
            resolve_scope(Some("/explicit/path")).unwrap(),
            "/explicit/path"
        );
        // "here" resolves to an absolute path (the test process's cwd).
        let here = resolve_scope(Some("here")).unwrap();
        assert!(std::path::Path::new(&here).is_absolute());
    }
}
