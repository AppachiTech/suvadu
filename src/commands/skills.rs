//! `suv skills` — CRUD, review, and native-format sync for the shared
//! cross-agent skills library (see `repository/skills.rs` for storage and
//! `skills_sync.rs` for materialization into each agent's own file format).

use crate::cli::SkillsCommands;
use crate::db;
use crate::models::{
    NewSkill, Skill, SKILL_SCOPE_GLOBAL, SKILL_SOURCE_HUMAN, SKILL_SOURCE_SUVADU,
    SKILL_STATUS_ACTIVE, SKILL_STATUS_ARCHIVED, SKILL_STATUS_PENDING,
};
use crate::repository::Repository;
use crate::skills_sync::{Destination, ALL_TARGETS};
use crate::util;
use std::fmt::Write as _;
use std::io::Read;

pub fn handle_skills(cmd: Option<SkillsCommands>) -> Result<(), Box<dyn std::error::Error>> {
    let repo = Repository::init()?;
    cmd.map_or_else(
        || crate::skills_ui::run(&repo),
        |cmd| handle_skills_with_repo(&repo, cmd),
    )
}

/// "global" (default), "here" (resolved to the current directory), or a
/// literal directory path passed through as-is.
fn resolve_scope(scope_arg: Option<&str>) -> Result<String, Box<dyn std::error::Error>> {
    match scope_arg {
        None | Some(SKILL_SCOPE_GLOBAL) => Ok(SKILL_SCOPE_GLOBAL.to_string()),
        Some("here") => Ok(std::env::current_dir()?.to_string_lossy().to_string()),
        Some(other) => Ok(crate::models::normalize_scope_path(other)),
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
        SkillsCommands::Disable { name, scope } => {
            handle_set_enabled(repo, &name, scope.as_deref(), false)
        }
        SkillsCommands::Enable { name, scope } => {
            handle_set_enabled(repo, &name, scope.as_deref(), true)
        }
        SkillsCommands::Sync {
            target,
            dry_run,
            force,
        } => handle_sync(repo, target, dry_run, force),
        SkillsCommands::Cleanup { target, dry_run } => handle_cleanup(repo, target, dry_run),
        SkillsCommands::Trust {
            name,
            repo: repo_path,
            dry_run,
        } => {
            let start = repo_start(repo_path.as_deref(), &std::env::current_dir()?);
            for line in trust_here(repo, &start, home().as_deref(), name.as_deref(), dry_run)? {
                println!("{line}");
            }
            Ok(())
        }
        SkillsCommands::Untrust {
            name,
            repo: repo_path,
        } => {
            let start = repo_start(repo_path.as_deref(), &std::env::current_dir()?);
            println!(
                "{}",
                untrust_here(repo, &start, home().as_deref(), name.as_deref())?
            );
            Ok(())
        }
    }
}

fn home() -> Option<std::path::PathBuf> {
    std::env::var_os("HOME").map(std::path::PathBuf::from)
}

/// Where `trust`/`untrust` look for repo skills: `--repo PATH` (with `~`
/// expanded, a relative path taken from `cwd`), or `cwd` itself.
fn repo_start(repo_arg: Option<&str>, cwd: &std::path::Path) -> std::path::PathBuf {
    repo_arg.map_or_else(
        || cwd.to_path_buf(),
        |p| cwd.join(crate::repository::expand_tilde(p)),
    )
}

/// The repo skills for the project around `start`, or an error saying none
/// were found.
fn require_discovery(
    repo: &Repository,
    start: &std::path::Path,
    home: Option<&std::path::Path>,
) -> Result<crate::repo_skills::Discovery, Box<dyn std::error::Error>> {
    crate::repo_skills::discover(repo, start, home)?.ok_or_else(|| {
        format!(
            "No repo skills for {}: no `.suvadu/skills/` directory there or above it \
             (up to the repository root).",
            start.display()
        )
        .into()
    })
}

/// `suv skills trust`: record the current content hash of a project's repo
/// skills (or the one named), so `suv skills sync` writes them.
pub fn trust_here(
    repo: &Repository,
    start: &std::path::Path,
    home: Option<&std::path::Path>,
    name: Option<&str>,
    dry_run: bool,
) -> Result<Vec<String>, Box<dyn std::error::Error>> {
    use crate::repo_skills::Trust;
    let found = require_discovery(repo, start, home)?;
    let mut out: Vec<String> = found
        .warnings
        .iter()
        .map(|w| format!("warning: {w}"))
        .collect();
    let chosen: Vec<&crate::repo_skills::RepoSkill> = match name {
        Some(name) => vec![found.find(name).ok_or_else(|| {
            format!(
                "No repo skill named '{name}' in {}/.suvadu/skills. Found: {}",
                found.root.display(),
                if found.skills.is_empty() {
                    "none".to_string()
                } else {
                    found
                        .skills
                        .iter()
                        .map(|s| s.skill.name.as_str())
                        .collect::<Vec<_>>()
                        .join(", ")
                }
            )
        })?],
        None => found.skills.iter().collect(),
    };
    if chosen.is_empty() {
        out.push(format!(
            "No usable skill files in {}/.suvadu/skills.",
            found.root.display()
        ));
        return Ok(out);
    }
    out.push(format!("Repo skills in {}:", found.root.display()));
    for s in &chosen {
        let verb = match (s.trust, dry_run) {
            (Trust::Trusted, _) => "already trusted",
            (Trust::Changed, true) => "would re-trust (changed)",
            (Trust::Changed, false) => "re-trusted (changed)",
            (Trust::Untrusted, true) => "would trust",
            (Trust::Untrusted, false) => "trusted",
        };
        out.push(format!(
            "  {verb:<24}  {:<24}  {}  sha256:{}",
            s.skill.name,
            s.rel_path,
            &s.hash[..12]
        ));
    }
    let pending = chosen.iter().filter(|s| s.trust != Trust::Trusted).count();
    if pending == 0 {
        out.push("\nNothing to trust — every one is trusted at its current content.".to_string());
        return Ok(out);
    }
    out.push(String::new());
    out.push(
        "These files came with the repository. Trusting one lets `suv skills sync` copy its text \
         into your agent files (Claude Code skills, Cursor rules, AGENTS.md) and lets connected \
         agents read it over MCP, so read each one first (`suv skills show <name>`). Any later \
         change to a file needs trusting again."
            .to_string(),
    );
    if dry_run {
        out.push(format!(
            "{pending} file(s) would be trusted. Re-run without --dry-run to trust them."
        ));
    } else {
        crate::repo_skills::trust(repo, &found, &chosen)?;
        out.push(format!(
            "{pending} file(s) trusted. Run `suv skills sync` from {} to write them.",
            found.root.display()
        ));
    }
    Ok(out)
}

/// `suv skills untrust`: forget the trust recorded for a project's repo
/// skills (or the one named). Their generated files go on the next sync.
/// Works on a root whose `.suvadu/skills/` has since been removed, too:
/// the trust recorded for it is still forgotten.
pub fn untrust_here(
    repo: &Repository,
    start: &std::path::Path,
    home: Option<&std::path::Path>,
    name: Option<&str>,
) -> Result<String, Box<dyn std::error::Error>> {
    let found = crate::repo_skills::discover(repo, start, home)?;
    let (scope, root) = found.as_ref().map_or_else(
        || {
            (
                crate::models::normalize_scope_path(&start.to_string_lossy()),
                start.to_path_buf(),
            )
        },
        |f| (f.scope(), f.root.clone()),
    );
    let removed = match name {
        Some(name) => {
            let s = found
                .as_ref()
                .and_then(|f| f.find(name))
                .ok_or_else(|| format!("No repo skill named '{name}' in {}.", root.display()))?;
            repo.untrust_repo_skills(&scope, Some(&s.rel_path))?
        }
        None => repo.untrust_repo_skills(&scope, None)?,
    };
    Ok(if removed == 0 {
        format!(
            "Nothing to untrust — no trust is recorded for repo skills in {}.",
            root.display()
        )
    } else {
        format!(
            "Stopped trusting {removed} repo skill file(s) in {}. The next `suv skills sync` there removes the agent files suvadu wrote for them.",
            root.display()
        )
    })
}

/// The error for a library operation on a name that only a repo skill has:
/// suvadu never edits, disables or deletes a file in a repository.
fn repo_only_error(found: Option<&crate::repo_skills::Discovery>, name: &str) -> Option<String> {
    let s = found?.find(name)?;
    Some(format!(
        "'{name}' is a repo skill from {} — it is read-only to suvadu. Edit or delete that file instead (and `suv skills untrust {name}` to stop it syncing).",
        s.path.display()
    ))
}

/// Where a repo skill would be written by a sync from `cwd`: nowhere until
/// it is trusted, and only to the agents its `targets:` names.
fn repo_destinations(
    rs: &crate::repo_skills::RepoSkill,
    cwd: &std::path::Path,
) -> Vec<Destination> {
    if rs.trust != crate::repo_skills::Trust::Trusted {
        return Vec::new();
    }
    let targets: Vec<crate::cli::SyncTarget> = ALL_TARGETS
        .into_iter()
        .filter(|&t| rs.syncs_to(t))
        .collect();
    crate::skills_sync::destinations_for(&rs.skill, &targets, cwd)
}

/// [`repo_destinations`] from the current directory.
fn repo_destinations_here(rs: &crate::repo_skills::RepoSkill) -> Vec<Destination> {
    std::env::current_dir().map_or_else(|_| Vec::new(), |cwd| repo_destinations(rs, &cwd))
}

/// The trusted repo skill that takes `skill`'s place in its project, if
/// any: a library skill scoped to the same project root with the same name.
pub fn shadowing_repo_skill<'a>(
    found: &'a crate::repo_skills::Discovery,
    skill: &Skill,
) -> Option<&'a crate::repo_skills::RepoSkill> {
    if skill.source == crate::models::SKILL_SOURCE_REPO || skill.scope != found.scope() {
        return None;
    }
    found.trusted().find(|rs| rs.skill.name == skill.name)
}

/// One `suv skills list` row for a repo skill: its trust state stands in
/// for the library state, since trust is what decides whether it syncs.
pub fn format_repo_list_row(rs: &crate::repo_skills::RepoSkill, name_width: usize) -> String {
    format!(
        "{:<width$} {:<14} {:<18} {:<20} {}",
        rs.skill.name,
        rs.trust.word(),
        "repo",
        util::truncate_str(&rs.rel_path, 20, "…"),
        util::truncate_str(&rs.skill.description, 50, "…"),
        width = name_width
    )
}

/// `suv skills show` for a repo skill: where its file is, whether it is
/// trusted, and where it syncs.
pub fn format_repo_skill_details(
    rs: &crate::repo_skills::RepoSkill,
    destinations: &[Destination],
) -> String {
    let s = &rs.skill;
    let mut out = format!("{}\n", s.name);
    let _ = writeln!(out, "  scope:    {}", s.scope);
    let _ = writeln!(out, "  file:     {}", rs.path.display());
    let _ = writeln!(out, "  origin:   {}", origin_label(&s.source));
    let _ = writeln!(out, "  trust:    {}", rs.trust.explain());
    if let Some(targets) = &rs.targets {
        let names: Vec<&str> = targets
            .iter()
            .map(|&t| crate::skills_sync::target_label(t))
            .collect();
        let _ = writeln!(out, "  targets:  {}", names.join(", "));
    }
    if !s.triggers.is_empty() {
        let _ = writeln!(out, "  triggers: {}", s.triggers.join(", "));
    }
    if !s.description.is_empty() {
        let _ = writeln!(out, "  summary:  {}", s.description);
    }
    if destinations.is_empty() {
        let reason = if rs.trust == crate::repo_skills::Trust::Trusted {
            format!("nothing from here — run `suv skills sync` from {}", s.scope)
        } else {
            "nothing until it is trusted".to_string()
        };
        let _ = writeln!(out, "  syncs to: {reason}");
    } else {
        let _ = writeln!(out, "  syncs to:");
        for d in destinations {
            let _ = writeln!(
                out,
                "    {} — {}",
                crate::skills_sync::target_label(d.target),
                d.describe()
            );
        }
    }
    let _ = write!(out, "\n{}", s.body);
    out
}

/// Targets to act on: the one the user named, or all of them.
fn targets_for(target: Option<crate::cli::SyncTarget>) -> Vec<crate::cli::SyncTarget> {
    target.map_or_else(|| ALL_TARGETS.to_vec(), |t| vec![t])
}

/// What a skill's `status` means in practice — whether it reaches agent
/// files at all, and what it takes to change that. The raw status word
/// (`pending_review`) doesn't say that nothing is being written.
pub fn state_label(status: &str) -> &'static str {
    match status {
        SKILL_STATUS_ACTIVE => "active — synced to agent files",
        SKILL_STATUS_PENDING => {
            "pending review — not synced until you approve it (`suv skills` → Ctrl+P)"
        }
        SKILL_STATUS_ARCHIVED => "disabled — not synced (`suv skills enable` to bring it back)",
        _ => "unknown status — not synced",
    }
}

/// Who authored a skill, in words rather than a raw `source` value.
pub fn origin_label(source: &str) -> String {
    match source {
        SKILL_SOURCE_HUMAN => "you".to_string(),
        SKILL_SOURCE_SUVADU => "suvadu (built-in, kept up to date for you)".to_string(),
        crate::models::SKILL_SOURCE_REPO => {
            "this repository (a file in .suvadu/skills/ — read-only to suvadu)".to_string()
        }
        other => other.strip_prefix("agent:").map_or_else(
            || other.to_string(),
            |agent| format!("proposed by agent '{agent}'"),
        ),
    }
}

/// The destinations a skill would be written to from the current directory,
/// or an empty list when it materializes nowhere.
fn destinations_here(skill: &Skill) -> Vec<Destination> {
    std::env::current_dir().map_or_else(
        |_| Vec::new(),
        |cwd| crate::skills_sync::destinations_for(skill, &ALL_TARGETS, &cwd),
    )
}

/// Why a skill has no destinations — the part a bare empty list can't say.
fn no_destination_reason(skill: &Skill) -> String {
    if skill.status != SKILL_STATUS_ACTIVE {
        return format!("nothing — {}", state_label(&skill.status));
    }
    if skill.scope == SKILL_SCOPE_GLOBAL {
        return "nothing — no target directory could be resolved (is HOME set?)".to_string();
    }
    format!(
        "nothing from here — this skill is scoped to {}; run `suv skills sync` from there",
        skill.scope
    )
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
    let found = crate::repo_skills::discover_here(repo)
        .filter(|f| scope_filter.as_deref().is_none_or(|s| s == f.scope()));
    let repo_skills: &[crate::repo_skills::RepoSkill] =
        found.as_ref().map_or(&[], |f| f.skills.as_slice());

    if json {
        let mut rows: Vec<serde_json::Value> = skills
            .iter()
            .map(serde_json::to_value)
            .collect::<Result<_, _>>()?;
        for rs in repo_skills {
            let mut value = serde_json::to_value(&rs.skill)?;
            value["path"] = rs.path.display().to_string().into();
            value["trust"] = rs.trust.word().into();
            rows.push(value);
        }
        println!("{}", serde_json::to_string_pretty(&rows)?);
        return Ok(());
    }

    if let Some(found) = &found {
        for w in &found.warnings {
            println!("warning: {w}");
        }
    }
    if skills.is_empty() && repo_skills.is_empty() {
        println!("No skills yet. Use `suv skills add <name>` to save one.");
        return Ok(());
    }

    let max_name = skills
        .iter()
        .map(|s| s.name.len())
        .chain(repo_skills.iter().map(|s| s.skill.name.len()))
        .max()
        .unwrap_or(4)
        .max(4);
    let header = format!(
        "{:<width$} {:<14} {:<18} {:<20} Description",
        "Name",
        "State",
        "Origin",
        "Scope",
        width = max_name
    );
    if util::color_enabled() {
        println!("\x1b[1m{header}\x1b[0m");
    } else {
        println!("{header}");
    }
    for s in &skills {
        println!("{}", format_list_row(s, max_name));
    }
    for rs in repo_skills {
        println!("{}", format_repo_list_row(rs, max_name));
    }
    println!("\n{} skill(s)", skills.len() + repo_skills.len());
    if repo_skills
        .iter()
        .any(|s| s.trust != crate::repo_skills::Trust::Trusted)
    {
        println!(
            "Repo skills (origin 'repo', from .suvadu/skills/) sync only once trusted: read them with `suv skills show <name>`, then `suv skills trust`."
        );
    }
    if skills.iter().any(|s| s.status != SKILL_STATUS_ACTIVE) {
        println!(
            "Only 'active' skills are written to agent files. `suv skills show <name>` lists the exact files for one skill."
        );
    } else {
        println!("`suv skills show <name>` lists the exact agent files a skill syncs to.");
    }
    Ok(())
}

/// One `suv skills list` row. State and origin sit next to the scope so the
/// list answers "is this live, and who wrote it?" without a second command.
pub fn format_list_row(s: &Skill, name_width: usize) -> String {
    let scope_display = if s.scope == SKILL_SCOPE_GLOBAL {
        SKILL_SCOPE_GLOBAL.to_string()
    } else {
        util::truncate_str(&s.scope, 18, "…")
    };
    let state = match s.status.as_str() {
        SKILL_STATUS_ACTIVE => "active",
        SKILL_STATUS_PENDING => "pending review",
        SKILL_STATUS_ARCHIVED => "disabled",
        other => other,
    };
    format!(
        "{:<width$} {:<14} {:<18} {:<20} {}",
        s.name,
        state,
        util::truncate_str(&origin_short(&s.source), 18, "…"),
        scope_display,
        util::truncate_str(&s.description, 50, "…"),
        width = name_width
    )
}

/// Column-width origin. Keeps the proposing agent's name readable — the
/// long [`origin_label`] phrasing truncates to "proposed by age…", which
/// hides exactly the part that matters in a review.
fn origin_short(source: &str) -> String {
    match source {
        SKILL_SOURCE_HUMAN => "you".to_string(),
        SKILL_SOURCE_SUVADU => "suvadu".to_string(),
        other => other.to_string(),
    }
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
        repo_only_error(crate::repo_skills::discover_here(repo).as_ref(), name)
            .unwrap_or_else(|| {
                format!(
                    "No active skill named '{name}' found. Pass --scope to target a specific one, or check `suv skills list --all`."
                )
            })
            .into()
    })
}

/// Everything `suv skills show` prints: where the skill applies, who wrote
/// it, whether it is live, and the exact agent files it is materialized
/// into — so "what will this change on disk?" is answerable before syncing.
pub fn format_skill_details(s: &Skill, destinations: &[Destination]) -> String {
    let mut out = format!("{}\n", s.name);
    let _ = writeln!(out, "  scope:    {}", s.scope);
    let _ = writeln!(out, "  state:    {}", state_label(&s.status));
    let _ = writeln!(out, "  origin:   {}", origin_label(&s.source));
    let _ = writeln!(out, "  version:  {}", s.version);
    if !s.triggers.is_empty() {
        let _ = writeln!(out, "  triggers: {}", s.triggers.join(", "));
    }
    if !s.description.is_empty() {
        let _ = writeln!(out, "  summary:  {}", s.description);
    }
    if destinations.is_empty() {
        let _ = writeln!(out, "  syncs to: {}", no_destination_reason(s));
    } else {
        let _ = writeln!(out, "  syncs to:");
        for d in destinations {
            let _ = writeln!(
                out,
                "    {} — {}",
                crate::skills_sync::target_label(d.target),
                d.describe()
            );
        }
    }
    let _ = write!(out, "\n{}", s.body);
    out
}

/// Resolve a skill for *reading*. Unlike [`resolve_target`] this also finds
/// pending and disabled ones — inspecting a proposal before approving it is
/// the whole point of the review step, and "no active skill named X" is a
/// confusing answer when X is sitting in the review queue.
fn resolve_for_display(
    repo: &Repository,
    name: &str,
    scope: Option<&str>,
) -> Result<Skill, Box<dyn std::error::Error>> {
    match resolve_target(repo, name, scope) {
        Ok(skill) => Ok(skill),
        Err(active_err) => resolve_any_status(repo, name, scope).map_err(|_| active_err),
    }
}

fn handle_show(
    repo: &Repository,
    name: &str,
    scope: Option<&str>,
) -> Result<(), Box<dyn std::error::Error>> {
    let found = crate::repo_skills::discover_here(repo);
    if scope.is_none() {
        if let Some(rs) = found.as_ref().and_then(|f| f.find(name)) {
            println!(
                "{}",
                format_repo_skill_details(rs, &repo_destinations_here(rs))
            );
            return Ok(());
        }
    }
    let skill = resolve_for_display(repo, name, scope)?;
    let mut text = format_skill_details(&skill, &destinations_here(&skill));
    if let Some(rs) = found.as_ref().and_then(|f| shadowing_repo_skill(f, &skill)) {
        text = text.replacen(
            "\n  syncs to:",
            &format!(
                "\n  shadowed: by the trusted repo file {} — that file is what syncs to this project\n  syncs to:",
                rs.path.display()
            ),
            1,
        );
    }
    println!("{text}");
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

/// What removing a skill from the library does *not* do. Deleting the row
/// does not touch the agent files generated from it — the next sync (or
/// cleanup) removes the ones suvadu can prove it wrote and nobody edited
/// since — so removal says plainly what is left behind for now and what
/// clears it.
pub fn removal_notice(name: &str, scope: &str, destinations: &[Destination]) -> String {
    let mut out = format!("✓ Skill '{name}' removed from the library ({scope})\n");
    if destinations.is_empty() {
        out.push_str("  No generated agent files were tracked for it from this directory.\n");
    } else {
        out.push_str("  Files generated from it are left in place for now:\n");
        for d in destinations {
            let _ = writeln!(out, "    {}", d.path.display());
        }
    }
    out.push_str(
        "  The next `suv skills sync` removes them, or run `suv skills cleanup` (`--dry-run`\n  to review first) to remove them now. Anything you edited by hand is kept and reported.",
    );
    out
}

fn handle_rm(
    repo: &Repository,
    name: &str,
    scope: Option<&str>,
) -> Result<(), Box<dyn std::error::Error>> {
    let existing = resolve_target(repo, name, scope)?;
    let destinations = destinations_here(&existing);
    if repo.delete_skill(&existing.name, &existing.scope)? {
        println!(
            "{}",
            removal_notice(&existing.name, &existing.scope, &destinations)
        );
    } else {
        println!(
            "Skill '{}' was already gone ({})",
            existing.name, existing.scope
        );
    }
    Ok(())
}

/// Disable (archive) or re-enable a skill — the reversible alternative to
/// `rm`. The content stays in the library; it just stops being written to
/// agent files.
fn handle_set_enabled(
    repo: &Repository,
    name: &str,
    scope: Option<&str>,
    enabled: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    // A disabled skill is invisible to `find_skill` (active-only), so
    // re-enabling has to resolve against every status instead.
    let existing = if enabled {
        resolve_any_status(repo, name, scope)?
    } else {
        resolve_target(repo, name, scope)?
    };
    // A proposal has exactly one way out of pending: a human decision in
    // the review queue, where its body is shown before it is accepted.
    // `enable` must not become a side door that activates an agent's
    // proposal without anyone reading it.
    if existing.status == SKILL_STATUS_PENDING {
        let verb = if enabled { "approve" } else { "reject" };
        return Err(format!(
            "'{name}' is a pending agent proposal, not an active skill. Review it in              `suv skills` (Ctrl+P) and {verb} it there — proposals are never activated              or archived without a review."
        )
        .into());
    }
    let status = if enabled {
        SKILL_STATUS_ACTIVE
    } else {
        SKILL_STATUS_ARCHIVED
    };
    let updated = repo
        .set_skill_status(&existing.name, &existing.scope, status)?
        .ok_or("skill disappeared while changing its state")?;
    if enabled {
        println!(
            "✓ Skill '{}' enabled ({}). Run `suv skills sync` to write it to your agent files.",
            updated.name, updated.scope
        );
    } else {
        println!(
            "✓ Skill '{}' disabled ({}) — kept in the library, no longer synced.\n  Already-generated files are removed by the next `suv skills sync` (or `suv skills cleanup` now); `suv skills enable {}` undoes this.",
            updated.name, updated.scope, updated.name
        );
    }
    Ok(())
}

/// Like [`resolve_target`] but also finds disabled and pending skills.
fn resolve_any_status(
    repo: &Repository,
    name: &str,
    scope: Option<&str>,
) -> Result<Skill, Box<dyn std::error::Error>> {
    if let Some(s) = scope {
        let scope = resolve_scope(Some(s))?;
        return repo
            .get_skill(name, &scope)?
            .ok_or_else(|| format!("No skill named '{name}' in scope '{scope}'.").into());
    }
    let mut matches: Vec<Skill> = repo
        .list_skills(None, None)?
        .into_iter()
        .filter(|s| s.name == name)
        .collect();
    match matches.len() {
        0 => Err(
            repo_only_error(crate::repo_skills::discover_here(repo).as_ref(), name)
                .unwrap_or_else(|| format!("No skill named '{name}'. See `suv skills list --all`."))
                .into(),
        ),
        1 => Ok(matches.remove(0)),
        _ => Err(format!(
            "'{name}' exists in several scopes ({}). Pass --scope to pick one.",
            matches
                .iter()
                .map(|s| s.scope.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        )
        .into()),
    }
}

/// A human's decision on one pending skill proposal.
pub enum ReviewDecision {
    Approve,
    Reject,
}

/// Apply a review decision to a pending skill. Shared between the TUI's
/// review queue (see `skills_ui.rs`) and this module's tests, so the
/// decision logic is defined once and tested once.
pub fn apply_review_decision(
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
    force: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    let targets = targets_for(target);
    let cwd = std::env::current_dir()?;

    // Seed/refresh suvadu-owned builtin skills before syncing, so a bare
    // `suv skills sync` installs them even without a prior
    // `suv init claude-code`. Config-load failures degrade to `Config`'s
    // defaults (matching `suv mcp-serve`'s own
    // `load_config().unwrap_or_default()` fallback) rather than blocking
    // sync of the user's own skills over a malformed config.toml — the
    // default has `allow_session_summaries = false`, i.e. the gate simply
    // reads as off. A real database error from `ensure_installed` still
    // propagates via `?`, matching how `suv skills sync` already fails
    // loudly on a real error (see the `?` on the sync call just below).
    // Gated on `!dry_run`: `ensure_installed` upserts into the skills
    // table, a real DB write, and `--dry-run` must never mutate state —
    // it only previews the file writes below. This means a first-time
    // `--dry-run` (before the skill has ever been seeded) won't preview
    // the new skill's rollout; accepted as a smaller cost than a
    // "dry run" flag silently writing persistent state.
    if !dry_run {
        let config = crate::config::load_config_cached().unwrap_or_default();
        crate::skills_builtin::ensure_installed(repo, &config)?;
    }

    let report = crate::skills_sync::sync(
        repo,
        &targets,
        &cwd,
        crate::skills_sync::SyncOptions { dry_run, force },
    )?;
    for line in report.lines(dry_run) {
        println!("{line}");
    }
    println!(
        "{}",
        sync_summary(
            report.written(),
            report.conflicts(),
            report.removed(),
            dry_run
        )
    );
    Ok(())
}

/// The closing line of a sync run: what happened, and what to do next when
/// something was left untouched.
pub fn sync_summary(written: usize, conflicts: usize, removed: usize, dry_run: bool) -> String {
    let mut out = if written == 0 && conflicts == 0 && removed == 0 {
        "\nNothing to sync — no active skills, or everything already up to date.".to_string()
    } else if written == 0 {
        // A blocked write is not an up-to-date file; saying so would hide
        // the one thing the user has to act on.
        String::new()
    } else if dry_run {
        format!("\n{written} file(s) would be written. Re-run without --dry-run to apply.")
    } else {
        format!("\n{written} file(s) written.")
    };
    if removed > 0 {
        let verb = if dry_run {
            "would be removed"
        } else {
            "removed"
        };
        let _ = write!(
            out,
            "\n{removed} generated file(s)/block(s) {verb} — the skill behind them is gone or no longer synced."
        );
    }
    if conflicts > 0 {
        let _ = write!(
            out,
            "\n{conflicts} file(s) left untouched because they were edited outside suvadu (marked CONFLICT above).\nFold the edit into the skill with `suv skills edit`, or re-run with --force to overwrite."
        );
    }
    out
}

/// `suv skills cleanup` — the deliberate counterpart to `rm`/`disable`.
fn handle_cleanup(
    repo: &Repository,
    target: Option<crate::cli::SyncTarget>,
    dry_run: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    let targets = targets_for(target);
    let cwd = std::env::current_dir()?;
    let report = crate::skills_sync::cleanup(repo, &targets, &cwd, dry_run)?;
    for line in report.lines(false) {
        println!("{line}");
    }
    println!(
        "{}",
        cleanup_summary(report.removed(), report.left_alone(), dry_run)
    );
    Ok(())
}

/// The closing line of a cleanup run.
///
/// `left_alone` is the number of files cleanup deliberately kept — generated
/// by suvadu, but edited since, so removing them would destroy that edit.
/// Removing nothing because everything is still in use and removing nothing
/// because edited files were preserved are different outcomes, and the
/// reassuring wording is only true of the first.
pub fn cleanup_summary(removed: usize, left_alone: usize, dry_run: bool) -> String {
    if removed == 0 {
        if left_alone > 0 {
            return format!(
                "\nNo files removed. {left_alone} generated file(s) were left alone because they \
                 were edited outside suvadu — see the reason against each above."
            );
        }
        return "\nNothing to clean up — every generated file still has an active skill behind it."
            .to_string();
    }
    if dry_run {
        format!("\n{removed} generated file(s)/block(s) would be removed. Re-run without --dry-run to apply.")
    } else {
        format!("\n{removed} generated file(s)/block(s) removed.")
    }
}

#[cfg(test)]
mod tests {
    /// Cleanup that removed nothing because it deliberately preserved an
    /// externally edited file must not claim every generated file still has
    /// an active skill behind it — it does not, which is why the file was
    /// left alone.
    #[test]
    fn cleanup_summary_does_not_claim_all_is_well_when_files_were_left_alone() {
        let summary = cleanup_summary(0, 1, false);
        assert!(
            !summary.contains("every generated file still has an active skill"),
            "left-alone files contradict that claim: {summary}"
        );
        assert!(
            summary.to_lowercase().contains("left alone") || summary.contains("above"),
            "the summary must point at the per-file explanation: {summary}"
        );
        // With nothing skipped the reassuring wording is still right.
        assert!(
            cleanup_summary(0, 0, false).contains("every generated file still has an active skill")
        );
    }

    use super::*;
    use crate::models::{SKILL_SOURCE_HUMAN, SKILL_STATUS_PENDING};
    use crate::test_utils::test_repo;

    fn repo_project(files: &[(&str, &str)]) -> tempfile::TempDir {
        let root = tempfile::tempdir().unwrap();
        let dir = root.path().join(".suvadu/skills");
        std::fs::create_dir_all(&dir).unwrap();
        for (name, content) in files {
            std::fs::write(dir.join(name), content).unwrap();
        }
        root
    }

    fn trust_states(repo: &Repository, root: &std::path::Path) -> Vec<(String, &'static str)> {
        crate::repo_skills::discover(repo, root, None)
            .unwrap()
            .unwrap()
            .skills
            .iter()
            .map(|s| (s.skill.name.clone(), s.trust.word()))
            .collect()
    }

    #[test]
    fn trust_dry_run_lists_without_recording_and_trust_records_each_file() {
        let (_dir, repo) = test_repo();
        let root = repo_project(&[("a.md", "Do a.\n"), ("b.md", "Do b.\n")]);

        let preview = trust_here(&repo, root.path(), None, None, true)
            .unwrap()
            .join("\n");
        assert!(preview.contains("would trust") && preview.contains(".suvadu/skills/a.md"));
        assert!(preview.contains("read each one first"), "{preview}");
        assert_eq!(trust_states(&repo, root.path())[0].1, "untrusted");

        let done = trust_here(&repo, root.path(), None, Some("a"), false)
            .unwrap()
            .join("\n");
        assert!(done.contains("1 file(s) trusted"), "{done}");
        assert_eq!(
            trust_states(&repo, root.path()),
            vec![("a".to_string(), "trusted"), ("b".to_string(), "untrusted")]
        );

        trust_here(&repo, root.path(), None, None, false).unwrap();
        let again = trust_here(&repo, root.path(), None, None, false)
            .unwrap()
            .join("\n");
        assert!(again.contains("Nothing to trust"), "{again}");

        std::fs::write(root.path().join(".suvadu/skills/b.md"), "Do b, changed.\n").unwrap();
        assert_eq!(trust_states(&repo, root.path())[1].1, "changed");
        let retrust = trust_here(&repo, root.path(), None, None, false)
            .unwrap()
            .join("\n");
        assert!(retrust.contains("re-trusted (changed)"), "{retrust}");
    }

    #[test]
    fn trust_works_from_a_subdirectory_or_a_repo_path() {
        let (_dir, repo) = test_repo();
        let root = repo_project(&[("a.md", "Do a.\n")]);
        std::fs::create_dir_all(root.path().join(".git")).unwrap();
        let sub = root.path().join("src/deep");
        std::fs::create_dir_all(&sub).unwrap();
        let elsewhere = tempfile::tempdir().unwrap();

        let start = repo_start(Some(&sub.to_string_lossy()), elsewhere.path());
        trust_here(&repo, &start, None, None, false).unwrap();
        assert_eq!(trust_states(&repo, root.path())[0].1, "trusted");

        // A relative --repo is taken from the current directory.
        let relative = repo_start(Some("src/deep"), root.path());
        assert_eq!(relative, sub);
        assert_eq!(repo_start(None, elsewhere.path()), elsewhere.path());
    }

    #[test]
    fn trust_errors_name_what_is_missing() {
        let (_dir, repo) = test_repo();
        let root = repo_project(&[("a.md", "Do a.\n")]);
        let err = trust_here(&repo, root.path(), None, Some("nope"), false).unwrap_err();
        assert!(err.to_string().contains("Found: a"), "{err}");

        let empty = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(empty.path().join(".git")).unwrap();
        let err = trust_here(&repo, empty.path(), None, None, false).unwrap_err();
        assert!(err.to_string().contains("No repo skills"), "{err}");
    }

    #[test]
    fn untrust_forgets_the_recorded_hashes() {
        let (_dir, repo) = test_repo();
        let root = repo_project(&[("a.md", "Do a.\n"), ("b.md", "Do b.\n")]);
        trust_here(&repo, root.path(), None, None, false).unwrap();
        let one = untrust_here(&repo, root.path(), None, Some("a")).unwrap();
        assert!(one.contains("1 repo skill file(s)"), "{one}");
        assert_eq!(
            trust_states(&repo, root.path()),
            vec![("a".to_string(), "untrusted"), ("b".to_string(), "trusted")]
        );
        untrust_here(&repo, root.path(), None, None).unwrap();
        let none = untrust_here(&repo, root.path(), None, None).unwrap();
        assert!(none.contains("Nothing to untrust"), "{none}");
    }

    #[test]
    fn untrust_still_works_once_the_skills_directory_is_gone() {
        let (_dir, repo) = test_repo();
        let root = repo_project(&[("a.md", "Do a.\n")]);
        trust_here(&repo, root.path(), None, None, false).unwrap();
        std::fs::remove_dir_all(root.path().join(".suvadu")).unwrap();
        let out = untrust_here(&repo, root.path(), None, None).unwrap();
        assert!(out.contains("1 repo skill file(s)"), "{out}");
        let scope = crate::models::normalize_scope_path(&root.path().to_string_lossy());
        assert!(repo.repo_skill_trust(&scope).unwrap().is_empty());
    }

    #[test]
    fn repo_skill_details_show_the_file_and_trust_and_list_rows_mark_the_origin() {
        let (_dir, repo) = test_repo();
        let root = repo_project(&[(
            "a.md",
            "---\ndescription: the a skill\ntargets: [codex]\n---\nDo a.\n",
        )]);
        let found = crate::repo_skills::discover(&repo, root.path(), None)
            .unwrap()
            .unwrap();
        let rs = &found.skills[0];
        let details = format_repo_skill_details(rs, &[]);
        assert!(
            details.contains(&rs.path.display().to_string()),
            "{details}"
        );
        assert!(details.contains("untrusted"), "{details}");
        assert!(details.contains("nothing until it is trusted"), "{details}");
        assert!(details.contains("read-only to suvadu"), "{details}");
        assert!(details.contains("targets:  Codex"), "{details}");
        let row = format_repo_list_row(rs, 4);
        assert!(row.contains("untrusted") && row.contains("repo"), "{row}");
    }

    #[test]
    fn a_trusted_repo_skill_destinations_follow_its_targets() {
        let (_dir, repo) = test_repo();
        let root = repo_project(&[("a.md", "---\ntargets: [cursor]\n---\nDo a.\n")]);
        trust_here(&repo, root.path(), None, None, false).unwrap();
        let found = crate::repo_skills::discover(&repo, root.path(), None)
            .unwrap()
            .unwrap();
        let dests = repo_destinations(&found.skills[0], root.path());
        assert_eq!(dests.len(), 1, "{dests:?}");
        assert_eq!(dests[0].target, crate::cli::SyncTarget::Cursor);
        let elsewhere = tempfile::tempdir().unwrap();
        assert!(repo_destinations(&found.skills[0], elsewhere.path()).is_empty());
    }

    #[test]
    fn a_trusted_repo_skill_shadows_the_library_project_skill_of_the_same_name() {
        let (_dir, repo) = test_repo();
        let root = repo_project(&[("deploy.md", "Repo version.\n")]);
        let scope = crate::models::normalize_scope_path(&root.path().to_string_lossy());
        let library = repo
            .create_skill(&NewSkill {
                name: "deploy".into(),
                description: String::new(),
                body: "Library version.".into(),
                triggers: Vec::new(),
                scope,
                source: SKILL_SOURCE_HUMAN.into(),
                status: SKILL_STATUS_ACTIVE.into(),
            })
            .unwrap();
        let found = crate::repo_skills::discover(&repo, root.path(), None)
            .unwrap()
            .unwrap();
        assert!(
            shadowing_repo_skill(&found, &library).is_none(),
            "untrusted: no effect"
        );
        trust_here(&repo, root.path(), None, None, false).unwrap();
        let found = crate::repo_skills::discover(&repo, root.path(), None)
            .unwrap()
            .unwrap();
        assert!(shadowing_repo_skill(&found, &library).is_some());
    }

    #[test]
    fn a_library_command_on_a_repo_only_name_points_at_the_file() {
        let (_dir, repo) = test_repo();
        let root = repo_project(&[("deploy.md", "Repo version.\n")]);
        let found = crate::repo_skills::discover(&repo, root.path(), None).unwrap();
        let err = repo_only_error(found.as_ref(), "deploy").unwrap();
        assert!(
            err.contains("read-only") && err.contains("deploy.md"),
            "{err}"
        );
        assert!(repo_only_error(found.as_ref(), "other").is_none());
    }

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

    #[test]
    fn state_label_spells_out_what_each_status_means_for_syncing() {
        assert!(state_label(SKILL_STATUS_ACTIVE).contains("synced"));
        let pending = state_label(SKILL_STATUS_PENDING);
        assert!(pending.contains("review"));
        assert!(pending.contains("not synced"));
        assert!(state_label(SKILL_STATUS_ARCHIVED).contains("disabled"));
    }

    #[test]
    fn list_keeps_the_proposing_agent_name_readable() {
        let (_dir, repo) = test_repo();
        let skill = repo
            .create_skill(&NewSkill {
                name: "proposed".into(),
                description: String::new(),
                body: "do X".into(),
                triggers: vec![],
                scope: SKILL_SCOPE_GLOBAL.into(),
                source: "agent:claude-code".into(),
                status: SKILL_STATUS_PENDING.into(),
            })
            .unwrap();
        let row = format_list_row(&skill, 8);
        assert!(
            row.contains("claude-code"),
            "the agent name must survive the Origin column: {row}"
        );
        assert!(row.contains("pending review"), "{row}");
    }

    #[test]
    fn origin_label_names_the_proposing_agent() {
        assert_eq!(origin_label(SKILL_SOURCE_HUMAN), "you");
        assert!(origin_label("agent:claude-code").contains("claude-code"));
        assert!(origin_label(crate::models::SKILL_SOURCE_SUVADU).contains("suvadu"));
    }

    #[test]
    fn skill_details_show_scope_origin_state_and_destinations() {
        let (_dir, repo) = test_repo();
        add(&repo, "deploy", None, "# steps").unwrap();
        let skill = resolve_target(&repo, "deploy", None).unwrap();
        let dests = vec![crate::skills_sync::Destination {
            target: crate::cli::SyncTarget::ClaudeCode,
            path: std::path::PathBuf::from("/home/u/.claude/skills/deploy/SKILL.md"),
            managed: crate::skills_sync::ManagedKind::WholeFile,
        }];

        let out = format_skill_details(&skill, &dests);
        assert!(out.contains("scope"));
        assert!(out.contains(SKILL_SCOPE_GLOBAL));
        assert!(out.contains("synced"), "state must be spelled out: {out}");
        assert!(out.contains("you"), "origin must be shown: {out}");
        assert!(out.contains("/home/u/.claude/skills/deploy/SKILL.md"));
        assert!(out.contains("# steps"));
    }

    #[test]
    fn skill_details_explain_why_a_pending_skill_syncs_nowhere() {
        let (_dir, repo) = test_repo();
        let skill = repo
            .create_skill(&NewSkill {
                name: "proposed".into(),
                description: String::new(),
                body: "do X".into(),
                triggers: vec![],
                scope: SKILL_SCOPE_GLOBAL.into(),
                source: "agent:codex".into(),
                status: SKILL_STATUS_PENDING.into(),
            })
            .unwrap();
        let out = format_skill_details(&skill, &[]);
        assert!(out.contains("not synced"), "{out}");
        assert!(out.contains("codex"), "{out}");
    }

    #[test]
    fn show_can_inspect_a_pending_proposal_not_just_active_skills() {
        let (_dir, repo) = test_repo();
        repo.create_skill(&NewSkill {
            name: "proposed".into(),
            description: String::new(),
            body: "do X".into(),
            triggers: vec![],
            scope: SKILL_SCOPE_GLOBAL.into(),
            source: "agent:codex".into(),
            status: SKILL_STATUS_PENDING.into(),
        })
        .unwrap();
        let skill = resolve_for_display(&repo, "proposed", None).unwrap();
        assert_eq!(skill.status, SKILL_STATUS_PENDING);
        assert!(resolve_target(&repo, "proposed", None).is_err());
    }

    #[test]
    fn sync_summary_does_not_claim_everything_is_up_to_date_when_a_conflict_blocked_it() {
        let out = sync_summary(0, 1, 0, false);
        assert!(
            !out.contains("Nothing to sync"),
            "a conflict is not 'up to date': {out}"
        );
        assert!(out.contains("CONFLICT"), "{out}");
        assert!(out.contains("--force"), "{out}");
    }

    #[test]
    fn sync_summary_reports_a_clean_no_op() {
        assert!(sync_summary(0, 0, 0, false).contains("Nothing to sync"));
    }

    #[test]
    fn sync_summary_counts_files_removed_for_skills_that_are_gone() {
        let out = sync_summary(0, 0, 2, false);
        assert!(!out.contains("Nothing to sync"), "{out}");
        assert!(
            out.contains("2 generated file(s)/block(s) removed"),
            "{out}"
        );
        let preview = sync_summary(1, 0, 2, true);
        assert!(preview.contains("would be removed"), "{preview}");
    }

    #[test]
    fn removal_notice_lists_leftover_files_and_points_at_cleanup() {
        let dests = vec![crate::skills_sync::Destination {
            target: crate::cli::SyncTarget::Cursor,
            path: std::path::PathBuf::from("/proj/.cursor/rules/gone.mdc"),
            managed: crate::skills_sync::ManagedKind::WholeFile,
        }];
        let notice = removal_notice("gone", SKILL_SCOPE_GLOBAL, &dests);
        assert!(notice.contains("/proj/.cursor/rules/gone.mdc"));
        assert!(
            notice.contains("suv skills cleanup"),
            "must name the deliberate cleanup command: {notice}"
        );
        assert!(
            notice.contains("left in place") || notice.contains("not removed"),
            "must say the files survive removal: {notice}"
        );
    }

    #[test]
    fn disable_then_enable_roundtrips_without_deleting_the_skill() {
        let (_dir, repo) = test_repo();
        add(&repo, "paused", None, "body").unwrap();

        handle_set_enabled(&repo, "paused", None, false).unwrap();
        let s = repo
            .get_skill("paused", SKILL_SCOPE_GLOBAL)
            .unwrap()
            .unwrap();
        assert_eq!(s.status, SKILL_STATUS_ARCHIVED);
        assert_eq!(s.body, "body", "disabling must not lose the content");

        handle_set_enabled(&repo, "paused", Some(SKILL_SCOPE_GLOBAL), true).unwrap();
        let s = repo
            .get_skill("paused", SKILL_SCOPE_GLOBAL)
            .unwrap()
            .unwrap();
        assert_eq!(s.status, SKILL_STATUS_ACTIVE);
    }

    #[test]
    fn enabling_a_pending_proposal_is_an_explicit_human_approval_not_a_sync_side_effect() {
        let (_dir, repo) = test_repo();
        repo.create_skill(&NewSkill {
            name: "proposed".into(),
            description: String::new(),
            body: "do X".into(),
            triggers: vec![],
            scope: SKILL_SCOPE_GLOBAL.into(),
            source: "agent:claude-code".into(),
            status: SKILL_STATUS_PENDING.into(),
        })
        .unwrap();

        // Nothing but an explicit decision moves it out of pending.
        let sync_report = crate::skills_sync::sync(
            &repo,
            &[],
            std::path::Path::new("/nowhere"),
            crate::skills_sync::SyncOptions::preview(),
        )
        .unwrap();
        assert_eq!(sync_report.written(), 0);
        assert_eq!(
            repo.get_skill("proposed", SKILL_SCOPE_GLOBAL)
                .unwrap()
                .unwrap()
                .status,
            SKILL_STATUS_PENDING
        );
    }

    #[test]
    fn list_row_shows_state_and_origin_next_to_the_scope() {
        let (_dir, repo) = test_repo();
        add(&repo, "deploy", None, "body").unwrap();
        let skill = resolve_target(&repo, "deploy", None).unwrap();
        let row = format_list_row(&skill, 8);
        assert!(row.contains("deploy"));
        assert!(row.contains("active"));
        assert!(row.contains("you"));
        assert!(row.contains(SKILL_SCOPE_GLOBAL));
    }
}
