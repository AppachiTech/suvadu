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
//!
//! Every generated region carries a checksum marker of what suvadu itself
//! wrote. That makes the relationship reversible in both directions: a later
//! sync can tell "suvadu wrote this and nobody touched it" from "somebody
//! edited this by hand", reporting the second as a conflict instead of
//! silently overwriting it, and [`cleanup`] can remove exactly the files
//! suvadu generated — and nothing else — once the skill behind them is gone.

use crate::cli::SyncTarget;
use crate::models::{Skill, SKILL_SCOPE_GLOBAL, SKILL_STATUS_ACTIVE};
use crate::repository::Repository;
use crate::util::atomic_write_with_mode;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

/// Every target `suv skills sync` writes to when none is named explicitly.
pub const ALL_TARGETS: [SyncTarget; 3] = [
    SyncTarget::ClaudeCode,
    SyncTarget::Cursor,
    SyncTarget::Codex,
];

const CODEX_BLOCK_START_PREFIX: &str = "<!-- suvadu:skills:start";
const CODEX_BLOCK_END: &str = "<!-- suvadu:skills:end -->";
const CODEX_BLOCK_NOTE: &str =
    "<!-- Managed by suvadu — edit via `suv skills edit`, then `suv skills sync`, not by hand. -->";

/// Trailing marker on a whole file suvadu generated. Records the skill it
/// came from (so [`cleanup`] can recognise its own output) and a checksum of
/// everything above it (so a later sync can tell an untouched generated file
/// from a hand-edited one).
const MANAGED_MARKER_PREFIX: &str = "<!-- suvadu:managed ";

/// How many diff lines a single file's preview may show before it is
/// truncated. Long enough for a realistic skill edit, short enough that a
/// first sync of a dozen skills stays readable in a terminal.
const MAX_DIFF_LINES: usize = 40;

/// Short, stable content fingerprint. Truncated sha256 — this detects
/// accidental hand edits, it is not a tamper-proof signature.
fn short_checksum(content: &str) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(content.as_bytes());
    format!("{:x}", hasher.finalize())[..16].to_string()
}

/// What a sync/cleanup would do (or did) to one file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChangeKind {
    /// The file (or managed block) does not exist yet.
    Create,
    /// Suvadu owns the region and its content changed.
    Update,
    /// Suvadu owns the region and it already matches — nothing written.
    Unchanged,
    /// The region was edited outside suvadu, or belongs to someone else.
    /// Never written without `--force`.
    Conflict,
    /// A generated file/block with no skill behind it any more.
    Removed,
    /// Present but deliberately left alone (e.g. no suvadu marker).
    Skipped,
}

impl ChangeKind {
    const fn label(self, dry_run: bool) -> &'static str {
        match self {
            Self::Create => {
                if dry_run {
                    "would create"
                } else {
                    "created"
                }
            }
            Self::Update => {
                if dry_run {
                    "would update"
                } else {
                    "updated"
                }
            }
            Self::Unchanged => "unchanged",
            Self::Conflict => "CONFLICT",
            Self::Removed => {
                if dry_run {
                    "would remove"
                } else {
                    "removed"
                }
            }
            Self::Skipped => "left alone",
        }
    }
}

/// One target file a sync or cleanup touched (or refused to touch).
#[derive(Debug, Clone)]
pub struct FileChange {
    pub target: SyncTarget,
    pub path: PathBuf,
    pub kind: ChangeKind,
    /// Names of the skills whose content this file carries.
    pub skills: Vec<String>,
    /// Line diff of the *managed* region only, `-`/`+` prefixed.
    pub diff: Vec<String>,
    /// Why, for conflicts and skips.
    pub detail: Option<String>,
}

/// The outcome of a sync or cleanup run: exactly which files changed, how,
/// and anything the user should know about how skills were resolved.
#[derive(Debug, Default)]
pub struct SyncReport {
    pub changes: Vec<FileChange>,
    /// Scope precedence decisions, skipped targets, and similar context.
    pub notes: Vec<String>,
    /// Whether this report describes a preview rather than applied writes.
    pub dry_run: bool,
}

impl SyncReport {
    fn count(&self, kind: ChangeKind) -> usize {
        self.changes.iter().filter(|c| c.kind == kind).count()
    }

    /// Files written (or that would be written under `--dry-run`).
    pub fn written(&self) -> usize {
        self.count(ChangeKind::Create) + self.count(ChangeKind::Update)
    }

    pub fn conflicts(&self) -> usize {
        self.count(ChangeKind::Conflict)
    }

    pub fn removed(&self) -> usize {
        self.count(ChangeKind::Removed)
    }

    fn push(&mut self, change: FileChange) {
        self.changes.push(change);
    }

    /// Human-readable rendering. `show_diff` includes the per-file managed
    /// diff — on by default for a preview, off for an applied sync where the
    /// user asked for the change and only needs the receipt.
    pub fn lines(&self, show_diff: bool) -> Vec<String> {
        let mut out: Vec<String> = self.notes.clone();
        let mut current: Option<SyncTarget> = None;
        for change in &self.changes {
            if current != Some(change.target) {
                out.push(format!("{}:", target_label(change.target)));
                current = Some(change.target);
            }
            let skills = if change.skills.is_empty() {
                String::new()
            } else {
                format!("  ({})", change.skills.join(", "))
            };
            out.push(format!(
                "  {:<12}  {}{}",
                change.kind.label(self.dry_run),
                change.path.display(),
                skills
            ));
            if let Some(detail) = &change.detail {
                out.push(format!("      ! {detail}"));
            }
            if show_diff || change.kind == ChangeKind::Conflict {
                for line in &change.diff {
                    out.push(format!("      {line}"));
                }
            }
        }
        out
    }
}

pub const fn target_label(target: SyncTarget) -> &'static str {
    match target {
        SyncTarget::ClaudeCode => "Claude Code",
        SyncTarget::Cursor => "Cursor (project rules — no global location to sync into)",
        SyncTarget::Codex => "Codex (AGENTS.md)",
    }
}

// ── Destinations ──────────────────────────────────────────────────────────

/// How suvadu owns a destination file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ManagedKind {
    /// The whole file is generated by suvadu.
    WholeFile,
    /// Only a marked block inside an otherwise hand-written file.
    Block,
}

/// One concrete file a skill is materialized into.
#[derive(Debug, Clone)]
pub struct Destination {
    pub target: SyncTarget,
    pub path: PathBuf,
    pub managed: ManagedKind,
}

impl Destination {
    pub fn describe(&self) -> String {
        let how = match self.managed {
            ManagedKind::WholeFile => "whole file",
            ManagedKind::Block => "managed block",
        };
        format!("{} ({how})", self.path.display())
    }
}

/// Exactly which agent files `skill` would be written to by a sync run from
/// `cwd`. Empty when the skill is project-scoped to a *different* directory,
/// or is not active — the two reasons a skill materializes nowhere.
pub fn destinations_for(skill: &Skill, targets: &[SyncTarget], cwd: &Path) -> Vec<Destination> {
    let home = std::env::var_os("HOME").map(PathBuf::from);
    let codex_home = std::env::var_os("CODEX_HOME")
        .filter(|p| !p.is_empty())
        .map(PathBuf::from);
    destinations_inner(skill, targets, cwd, home.as_deref(), codex_home.as_deref())
}

fn destinations_inner(
    skill: &Skill,
    targets: &[SyncTarget],
    cwd: &Path,
    home: Option<&Path>,
    codex_home_override: Option<&Path>,
) -> Vec<Destination> {
    let is_global = skill.scope == SKILL_SCOPE_GLOBAL;
    if skill.status != SKILL_STATUS_ACTIVE {
        return Vec::new();
    }
    if !is_global && skill.scope != cwd.to_string_lossy() {
        return Vec::new();
    }

    let mut out = Vec::new();
    for &target in targets {
        match target {
            SyncTarget::ClaudeCode => {
                let root = if is_global {
                    home.map(|h| h.join(".claude").join("skills"))
                } else {
                    Some(cwd.join(".claude").join("skills"))
                };
                if let Some(root) = root {
                    out.push(Destination {
                        target,
                        path: root.join(&skill.name).join("SKILL.md"),
                        managed: ManagedKind::WholeFile,
                    });
                }
            }
            SyncTarget::Cursor => out.push(Destination {
                target,
                path: cursor_rule_path(cwd, &skill.name),
                managed: ManagedKind::WholeFile,
            }),
            SyncTarget::Codex => {
                let path = if is_global {
                    codex_home_path(home, codex_home_override).map(|h| h.join("AGENTS.md"))
                } else {
                    Some(cwd.join("AGENTS.md"))
                };
                if let Some(path) = path {
                    out.push(Destination {
                        target,
                        path,
                        managed: ManagedKind::Block,
                    });
                }
            }
        }
    }
    out
}

fn cursor_rule_path(cwd: &Path, name: &str) -> PathBuf {
    cwd.join(".cursor")
        .join("rules")
        .join(format!("{name}.mdc"))
}

fn codex_home_path(home: Option<&Path>, override_path: Option<&Path>) -> Option<PathBuf> {
    override_path
        .map(Path::to_path_buf)
        .or_else(|| home.map(|h| h.join(".codex")))
}

// ── Sync ──────────────────────────────────────────────────────────────────

/// Knobs for one sync run.
#[derive(Debug, Clone, Copy, Default)]
pub struct SyncOptions {
    /// Preview only: report every change without writing anything.
    pub dry_run: bool,
    /// Overwrite regions that were edited outside suvadu. Off by default —
    /// a conflict is reported and skipped instead.
    pub force: bool,
}

impl SyncOptions {
    pub const fn apply() -> Self {
        Self {
            dry_run: false,
            force: false,
        }
    }

    /// A preview run. Only tests construct one directly — the CLI builds
    /// its options from `--dry-run`/`--force` together.
    #[cfg(test)]
    pub const fn preview() -> Self {
        Self {
            dry_run: true,
            force: false,
        }
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
    options: SyncOptions,
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
        options,
    )
}

/// Resolve which skills a sync from `cwd` applies to, and how scopes that
/// collide are decided. A project-scoped skill shadows a global skill of the
/// same name — the same precedence `Repository::find_skill` (and therefore
/// `suv skills show`) already applies, so what the CLI shows you and what
/// gets written to your agent files can never disagree.
fn resolve_for_cwd<'a>(all: &'a [Skill], cwd_str: &str) -> (Vec<&'a Skill>, Vec<String>) {
    let relevant: Vec<&Skill> = all
        .iter()
        .filter(|s| s.scope == SKILL_SCOPE_GLOBAL || s.scope == cwd_str)
        .collect();
    let project_names: Vec<&str> = relevant
        .iter()
        .filter(|s| s.scope != SKILL_SCOPE_GLOBAL)
        .map(|s| s.name.as_str())
        .collect();

    let mut notes = Vec::new();
    let mut kept = Vec::new();
    for skill in relevant {
        if skill.scope == SKILL_SCOPE_GLOBAL && project_names.contains(&skill.name.as_str()) {
            notes.push(format!(
                "note: project-scoped '{}' shadows the global skill of the same name here — the project one is written, the global one is skipped.",
                skill.name
            ));
        } else {
            kept.push(skill);
        }
    }
    (kept, notes)
}

fn sync_inner(
    repo: &Repository,
    targets: &[SyncTarget],
    cwd: &Path,
    home: Option<&Path>,
    codex_home_override: Option<&Path>,
    options: SyncOptions,
) -> Result<SyncReport, Box<dyn std::error::Error>> {
    let all = repo.list_skills(None, Some(SKILL_STATUS_ACTIVE))?;
    let cwd_str = cwd.to_string_lossy().to_string();
    let (relevant, notes) = resolve_for_cwd(&all, &cwd_str);

    let mut report = SyncReport {
        changes: Vec::new(),
        notes,
        dry_run: options.dry_run,
    };
    if relevant.is_empty() {
        return Ok(report);
    }

    for &target in targets {
        match target {
            SyncTarget::ClaudeCode => {
                sync_claude_code(&relevant, home, cwd, options, &mut report)?;
            }
            SyncTarget::Cursor => sync_cursor(&relevant, cwd, options, &mut report)?,
            SyncTarget::Codex => {
                sync_codex(
                    &relevant,
                    home,
                    codex_home_override,
                    cwd,
                    options,
                    &mut report,
                )?;
            }
        }
    }

    Ok(report)
}

// ── Whole-file targets (Claude Code, Cursor) ──────────────────────────────

/// What suvadu's ownership of an existing file looks like.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Ownership {
    Absent,
    /// Written by suvadu and untouched since.
    Ours,
    /// Written by suvadu, then edited by hand.
    Drifted,
    /// Suvadu's region is intact, but somebody wrote below the marker.
    /// Rewriting the file would delete that text, so it is a conflict too.
    Appended,
    /// No suvadu marker and not recognisable as suvadu output.
    Foreign,
}

/// Wrap generated `base` content in suvadu's trailing ownership marker.
fn managed_file_content(base: &str, skill_name: &str) -> String {
    let before = format!("{base}\n");
    let sum = short_checksum(&before);
    format!("{before}{MANAGED_MARKER_PREFIX}skill={skill_name} checksum={sum} -->\n")
}

/// Split a managed file into (everything above the marker, recorded
/// checksum, everything below the marker line). The third part is the one
/// suvadu's checksum says nothing about: it exists only because somebody
/// wrote there, and a whole-file rewrite would delete it.
fn split_managed(existing: &str) -> Option<(&str, &str, &str)> {
    let idx = existing.rfind(MANAGED_MARKER_PREFIX)?;
    let before = &existing[..idx];
    let line_end = existing[idx..]
        .find('\n')
        .map_or(existing.len(), |off| idx + off);
    let sum = existing[idx..line_end]
        .split_whitespace()
        .find_map(|tok| tok.strip_prefix("checksum="))?;
    let after = existing.get(line_end + 1..).unwrap_or_default();
    Some((before, sum, after))
}

/// The user-visible content of an existing marked file: everything except
/// suvadu's own marker line. Trailing content counts — a diff that hides it
/// would promise a clean update while a write silently deletes it. For an
/// unmarked file it is the whole file.
fn managed_region(existing: &str) -> String {
    split_managed(existing).map_or_else(
        || existing.to_string(),
        |(before, _, after)| {
            if after.is_empty() {
                return before.to_string();
            }
            // The blank line suvadu leaves above its marker is bookkeeping,
            // not content — dropping it here keeps a diff of appended text
            // free of blank `-` lines nobody wrote.
            format!("{}\n{after}", before.trim_end_matches('\n'))
        },
    )
}

/// Ownership of a file that carries suvadu's marker. `None` when there is
/// no marker at all.
fn marked_ownership(existing: &str) -> Option<Ownership> {
    let (before, sum, after) = split_managed(existing)?;
    Some(if short_checksum(before) != sum {
        Ownership::Drifted
    } else if after.trim().is_empty() {
        Ownership::Ours
    } else {
        // The marker is terminal by construction, so anything below it was
        // added afterwards by a person or another tool.
        Ownership::Appended
    })
}

fn classify_file(existing: Option<&str>, legacy_base: &str) -> Ownership {
    let Some(existing) = existing else {
        return Ownership::Absent;
    };
    marked_ownership(existing).unwrap_or({
        // Pre-marker suvadu output, still byte-identical to what this
        // generator produces: adopt it silently rather than calling the
        // user's own file a conflict on the first upgraded sync.
        if existing == legacy_base {
            Ownership::Ours
        } else {
            Ownership::Foreign
        }
    })
}

const DRIFT_DETAIL: &str = "this file was edited outside suvadu since the last sync — not written. Fold your edit into the skill (`suv skills edit`), or re-run with --force to overwrite it.";
const FOREIGN_DETAIL: &str = "this file was not written by suvadu (no managed marker) — not written. Move it aside, or re-run with --force to replace it.";
const APPENDED_DETAIL: &str = "this file has content below suvadu's managed marker — not written, because suvadu owns the whole file and rewriting it would delete that text (the diff below shows it as removed). Move it into the skill (`suv skills edit`), or re-run with --force to discard it.";

fn sync_whole_file(
    target: SyncTarget,
    skill: &Skill,
    path: &Path,
    base: &str,
    options: SyncOptions,
    report: &mut SyncReport,
) -> Result<(), Box<dyn std::error::Error>> {
    let existing = std::fs::read_to_string(path).ok();
    let desired = managed_file_content(base, &skill.name);
    let ownership = classify_file(existing.as_deref(), base);
    let old_region = existing.as_deref().map(managed_region).unwrap_or_default();
    let new_region = format!("{base}\n");
    let diff = diff_lines(&old_region, &new_region);

    let mut change = FileChange {
        target,
        path: path.to_path_buf(),
        kind: ChangeKind::Unchanged,
        skills: vec![skill.name.clone()],
        diff,
        detail: None,
    };

    match ownership {
        Ownership::Drifted | Ownership::Foreign | Ownership::Appended if !options.force => {
            change.kind = ChangeKind::Conflict;
            change.detail = Some(
                match ownership {
                    Ownership::Drifted => DRIFT_DETAIL,
                    Ownership::Appended => APPENDED_DETAIL,
                    _ => FOREIGN_DETAIL,
                }
                .to_string(),
            );
            report.push(change);
            return Ok(());
        }
        Ownership::Ours if existing.as_deref() == Some(desired.as_str()) => {
            change.kind = ChangeKind::Unchanged;
            change.diff.clear();
            report.push(change);
            return Ok(());
        }
        _ => {}
    }

    change.kind = if existing.is_none() {
        ChangeKind::Create
    } else {
        ChangeKind::Update
    };
    if !options.dry_run {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        atomic_write_with_mode(path, &desired, 0o600)?;
    }
    report.push(change);
    Ok(())
}

fn claude_base(skill: &Skill) -> String {
    format!(
        "---\nname: {}\ndescription: {}\n---\n\n{}\n",
        skill.name,
        escape_yaml_scalar(&skill.description),
        skill.body.trim_end()
    )
}

fn cursor_base(skill: &Skill) -> String {
    format!(
        "---\ndescription: {}\nalwaysApply: false\n---\n\n{}\n",
        escape_yaml_scalar(&skill.description),
        skill.body.trim_end()
    )
}

fn sync_claude_code(
    skills: &[&Skill],
    home: Option<&Path>,
    cwd: &Path,
    options: SyncOptions,
    report: &mut SyncReport,
) -> Result<(), Box<dyn std::error::Error>> {
    let Some(home) = home else {
        report
            .notes
            .push("skipped claude-code (HOME not set)".to_string());
        return Ok(());
    };
    let global_root = home.join(".claude").join("skills");
    let project_root = cwd.join(".claude").join("skills");

    for skill in skills {
        let dir = skill_target_dir(skill, &global_root, &project_root).join(&skill.name);
        let path = dir.join("SKILL.md");
        sync_whole_file(
            SyncTarget::ClaudeCode,
            skill,
            &path,
            &claude_base(skill),
            options,
            report,
        )?;
    }
    Ok(())
}

fn skill_target_dir(skill: &Skill, global_root: &Path, project_root: &Path) -> PathBuf {
    if skill.scope == SKILL_SCOPE_GLOBAL {
        global_root.to_path_buf()
    } else {
        project_root.to_path_buf()
    }
}

fn sync_cursor(
    skills: &[&Skill],
    cwd: &Path,
    options: SyncOptions,
    report: &mut SyncReport,
) -> Result<(), Box<dyn std::error::Error>> {
    // Cursor has no stable global rules location, so both global and
    // project-scoped skills land in this project's .cursor/rules/.
    for skill in skills {
        let path = cursor_rule_path(cwd, &skill.name);
        sync_whole_file(
            SyncTarget::Cursor,
            skill,
            &path,
            &cursor_base(skill),
            options,
            report,
        )?;
    }
    Ok(())
}

// ── Codex: one managed block inside AGENTS.md per scope ────────────────────

fn sync_codex(
    skills: &[&Skill],
    home: Option<&Path>,
    codex_home_override: Option<&Path>,
    cwd: &Path,
    options: SyncOptions,
    report: &mut SyncReport,
) -> Result<(), Box<dyn std::error::Error>> {
    let global: Vec<&&Skill> = skills
        .iter()
        .filter(|s| s.scope == SKILL_SCOPE_GLOBAL)
        .collect();
    let project: Vec<&&Skill> = skills
        .iter()
        .filter(|s| s.scope != SKILL_SCOPE_GLOBAL)
        .collect();

    if !global.is_empty() {
        if let Some(codex_home) = codex_home_path(home, codex_home_override) {
            sync_codex_file(&codex_home.join("AGENTS.md"), &global, options, report)?;
        } else {
            report
                .notes
                .push("skipped codex global (HOME/CODEX_HOME not set)".to_string());
        }
    }
    if !project.is_empty() {
        sync_codex_file(&cwd.join("AGENTS.md"), &project, options, report)?;
    }
    Ok(())
}

/// The content of a managed block *below* its start-marker line, up to and
/// including the end marker. This is what the checksum covers.
fn codex_block_inner(skills: &[&&Skill]) -> String {
    let mut block = String::new();
    block.push_str(CODEX_BLOCK_NOTE);
    block.push('\n');
    for skill in skills {
        let _ = write!(block, "\n## {}\n\n", skill.name);
        if !skill.description.is_empty() {
            let _ = writeln!(block, "{}\n", skill.description);
        }
        block.push_str(skill.body.trim_end());
        block.push('\n');
    }
    block.push_str(CODEX_BLOCK_END);
    block
}

fn codex_block(skills: &[&&Skill]) -> String {
    let inner = codex_block_inner(skills);
    format!(
        "{CODEX_BLOCK_START_PREFIX} checksum={} -->\n{inner}",
        short_checksum(&inner)
    )
}

/// A managed block located inside an existing file.
struct FoundBlock {
    start: usize,
    end: usize,
    inner_start: usize,
    checksum: Option<String>,
}

fn find_codex_block(existing: &str) -> Option<FoundBlock> {
    let start = existing.find(CODEX_BLOCK_START_PREFIX)?;
    let end_rel = existing[start..].find(CODEX_BLOCK_END)?;
    let end = start + end_rel + CODEX_BLOCK_END.len();
    let line_end = existing[start..end].find('\n').map(|off| start + off)?;
    let checksum = existing[start..line_end]
        .split_whitespace()
        .find_map(|tok| tok.strip_prefix("checksum="))
        .map(ToString::to_string);
    Some(FoundBlock {
        start,
        end,
        inner_start: line_end + 1,
        checksum,
    })
}

fn sync_codex_file(
    path: &Path,
    skills: &[&&Skill],
    options: SyncOptions,
    report: &mut SyncReport,
) -> Result<(), Box<dyn std::error::Error>> {
    let existing = std::fs::read_to_string(path).unwrap_or_default();
    let found = find_codex_block(&existing);
    let desired_inner = codex_block_inner(skills);
    let old_inner = found
        .as_ref()
        .map_or("", |b| &existing[b.inner_start..b.end]);

    let mut change = FileChange {
        target: SyncTarget::Codex,
        path: path.to_path_buf(),
        kind: ChangeKind::Unchanged,
        skills: skills.iter().map(|s| s.name.clone()).collect(),
        diff: diff_lines(old_inner, &desired_inner),
        detail: None,
    };

    // A block with no checksum predates the marker format. It still carries
    // suvadu's own "managed by suvadu" banner, so it is unambiguously ours —
    // adopt it rather than reporting a conflict on the first upgraded sync.
    if let Some(block) = &found {
        if let Some(recorded) = &block.checksum {
            if *recorded != short_checksum(old_inner) && !options.force {
                change.kind = ChangeKind::Conflict;
                change.detail = Some(format!(
                    "the suvadu-managed block in this file was edited outside suvadu — not written. Everything outside the block is untouched either way. {DRIFT_DETAIL}"
                ));
                report.push(change);
                return Ok(());
            }
        }
    }

    let new_block = codex_block(skills);
    let updated = found.as_ref().map_or_else(
        || append_block(&existing, &new_block),
        |b| {
            let mut out = String::with_capacity(existing.len() + new_block.len());
            out.push_str(&existing[..b.start]);
            out.push_str(&new_block);
            out.push_str(&existing[b.end..]);
            out
        },
    );

    if updated == existing {
        change.kind = ChangeKind::Unchanged;
        change.diff.clear();
        report.push(change);
        return Ok(());
    }
    change.kind = if found.is_none() {
        ChangeKind::Create
    } else {
        ChangeKind::Update
    };
    if !options.dry_run {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        atomic_write_with_mode(path, &updated, 0o600)?;
    }
    report.push(change);
    Ok(())
}

/// Append `new_block` below any existing content, leaving it untouched.
fn append_block(existing: &str, new_block: &str) -> String {
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

/// Remove a managed block (and the blank line it left behind) from `existing`.
fn remove_codex_block(existing: &str, block: &FoundBlock) -> String {
    let mut out = String::with_capacity(existing.len());
    out.push_str(&existing[..block.start]);
    let rest = existing[block.end..].trim_start_matches('\n');
    if !out.is_empty() && !rest.is_empty() {
        while out.ends_with("\n\n\n") {
            out.pop();
        }
        out.push_str(rest);
    } else if rest.is_empty() {
        while out.ends_with("\n\n") {
            out.pop();
        }
    } else {
        out.push_str(rest);
    }
    out
}

// ── Cleanup ───────────────────────────────────────────────────────────────

/// Remove agent files suvadu generated for skills that are no longer active
/// — the deliberate counterpart to `suv skills rm`/`disable`, which only
/// change the library. Only ever touches regions carrying suvadu's own
/// managed marker; anything else is reported and left in place.
pub fn cleanup(
    repo: &Repository,
    targets: &[SyncTarget],
    cwd: &Path,
    dry_run: bool,
) -> Result<SyncReport, Box<dyn std::error::Error>> {
    let home = std::env::var_os("HOME").map(PathBuf::from);
    let codex_home_override = std::env::var_os("CODEX_HOME")
        .filter(|p| !p.is_empty())
        .map(PathBuf::from);
    cleanup_inner(
        repo,
        targets,
        cwd,
        home.as_deref(),
        codex_home_override.as_deref(),
        dry_run,
    )
}

fn cleanup_inner(
    repo: &Repository,
    targets: &[SyncTarget],
    cwd: &Path,
    home: Option<&Path>,
    codex_home_override: Option<&Path>,
    dry_run: bool,
) -> Result<SyncReport, Box<dyn std::error::Error>> {
    let all = repo.list_skills(None, Some(SKILL_STATUS_ACTIVE))?;
    let cwd_str = cwd.to_string_lossy().to_string();
    let (relevant, _) = resolve_for_cwd(&all, &cwd_str);

    // Global destinations are shared by every project, so what belongs in
    // them is decided by the active *global* library entries — before any
    // project-scoped shadowing. `relevant` has shadowed globals removed for
    // this cwd, which is right for deciding what to write here but wrong
    // for deciding what is orphaned: a project override of `alpha` must
    // never uninstall global `alpha` for every other directory.
    let global_names: Vec<&str> = all
        .iter()
        .filter(|s| s.scope == SKILL_SCOPE_GLOBAL)
        .map(|s| s.name.as_str())
        .collect();

    let mut report = SyncReport {
        changes: Vec::new(),
        notes: Vec::new(),
        dry_run,
    };

    for &target in targets {
        match target {
            SyncTarget::ClaudeCode => {
                if let Some(home) = home {
                    cleanup_claude_root(
                        &home.join(".claude").join("skills"),
                        &global_names,
                        dry_run,
                        &mut report,
                    )?;
                }
                let expected: Vec<&str> = relevant
                    .iter()
                    .filter(|s| s.scope != SKILL_SCOPE_GLOBAL)
                    .map(|s| s.name.as_str())
                    .collect();
                cleanup_claude_root(
                    &cwd.join(".claude").join("skills"),
                    &expected,
                    dry_run,
                    &mut report,
                )?;
            }
            SyncTarget::Cursor => {
                // Cursor rules live in this project only and a shadowed
                // global always has a same-named project skill writing the
                // same file, so the cwd-resolved set is complete here.
                let expected: Vec<&str> = relevant.iter().map(|s| s.name.as_str()).collect();
                cleanup_cursor(
                    &cwd.join(".cursor").join("rules"),
                    &expected,
                    dry_run,
                    &mut report,
                )?;
            }
            SyncTarget::Codex => {
                let has_global = !global_names.is_empty();
                if let Some(codex_home) = codex_home_path(home, codex_home_override) {
                    cleanup_codex_file(
                        &codex_home.join("AGENTS.md"),
                        has_global,
                        dry_run,
                        &mut report,
                    )?;
                }
                let has_project = relevant.iter().any(|s| s.scope != SKILL_SCOPE_GLOBAL);
                cleanup_codex_file(&cwd.join("AGENTS.md"), has_project, dry_run, &mut report)?;
            }
        }
    }
    Ok(report)
}

const FOREIGN_CLEANUP_DETAIL: &str =
    "no suvadu managed marker — suvadu did not write this, so it is left in place. Remove it by hand if you no longer want it.";
const DRIFTED_CLEANUP_DETAIL: &str =
    "suvadu wrote this but it was edited afterwards — left in place so your edit isn't lost. Remove it by hand once you've saved what you need.";
const APPENDED_CLEANUP_DETAIL: &str =
    "suvadu wrote this but text was added below its managed marker — left in place so that text isn't lost. Remove it by hand once you've saved what you need.";

/// Delete `path` if suvadu generated it and no active skill still wants it.
fn cleanup_generated_file(
    target: SyncTarget,
    path: &Path,
    skill_name: &str,
    still_wanted: bool,
    dry_run: bool,
    report: &mut SyncReport,
) -> Result<(), Box<dyn std::error::Error>> {
    if still_wanted {
        return Ok(());
    }
    let Ok(content) = std::fs::read_to_string(path) else {
        return Ok(());
    };
    // The same complete ownership check `sync` uses: suvadu only deletes a
    // file whose marker is intact *and* terminal.
    let detail = match marked_ownership(&content) {
        Some(Ownership::Ours) => None,
        Some(Ownership::Appended) => Some(APPENDED_CLEANUP_DETAIL),
        Some(_) => Some(DRIFTED_CLEANUP_DETAIL),
        None => Some(FOREIGN_CLEANUP_DETAIL),
    };
    if let Some(detail) = detail {
        report.push(FileChange {
            target,
            path: path.to_path_buf(),
            kind: ChangeKind::Skipped,
            skills: vec![skill_name.to_string()],
            diff: Vec::new(),
            detail: Some(detail.to_string()),
        });
        return Ok(());
    }
    if !dry_run {
        std::fs::remove_file(path)?;
        // Take the now-empty per-skill directory with it (Claude Code only —
        // `remove_dir` refuses a non-empty directory, so this is a no-op for
        // a shared rules directory).
        if let Some(dir) = path.parent() {
            let _ = std::fs::remove_dir(dir);
        }
    }
    report.push(FileChange {
        target,
        path: path.to_path_buf(),
        kind: ChangeKind::Removed,
        skills: vec![skill_name.to_string()],
        diff: Vec::new(),
        detail: None,
    });
    Ok(())
}

fn cleanup_claude_root(
    root: &Path,
    expected: &[&str],
    dry_run: bool,
    report: &mut SyncReport,
) -> Result<(), Box<dyn std::error::Error>> {
    let Ok(entries) = std::fs::read_dir(root) else {
        return Ok(());
    };
    let mut dirs: Vec<PathBuf> = entries
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| p.is_dir())
        .collect();
    dirs.sort();
    for dir in dirs {
        let Some(name) = dir.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        let name = name.to_string();
        cleanup_generated_file(
            SyncTarget::ClaudeCode,
            &dir.join("SKILL.md"),
            &name,
            expected.contains(&name.as_str()),
            dry_run,
            report,
        )?;
    }
    Ok(())
}

fn cleanup_cursor(
    rules_dir: &Path,
    expected: &[&str],
    dry_run: bool,
    report: &mut SyncReport,
) -> Result<(), Box<dyn std::error::Error>> {
    let Ok(entries) = std::fs::read_dir(rules_dir) else {
        return Ok(());
    };
    let mut files: Vec<PathBuf> = entries
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|e| e == "mdc"))
        .collect();
    files.sort();
    for file in files {
        let Some(name) = file.file_stem().and_then(|n| n.to_str()) else {
            continue;
        };
        let name = name.to_string();
        cleanup_generated_file(
            SyncTarget::Cursor,
            &file,
            &name,
            expected.contains(&name.as_str()),
            dry_run,
            report,
        )?;
    }
    Ok(())
}

/// Strip an orphaned managed block out of an `AGENTS.md`, leaving every
/// hand-written line around it exactly as it was.
fn cleanup_codex_file(
    path: &Path,
    still_wanted: bool,
    dry_run: bool,
    report: &mut SyncReport,
) -> Result<(), Box<dyn std::error::Error>> {
    if still_wanted {
        return Ok(());
    }
    let Ok(existing) = std::fs::read_to_string(path) else {
        return Ok(());
    };
    let Some(block) = find_codex_block(&existing) else {
        return Ok(());
    };
    let old_inner = &existing[block.inner_start..block.end];
    if let Some(recorded) = &block.checksum {
        if *recorded != short_checksum(old_inner) {
            report.push(FileChange {
                target: SyncTarget::Codex,
                path: path.to_path_buf(),
                kind: ChangeKind::Skipped,
                skills: Vec::new(),
                diff: Vec::new(),
                detail: Some(
                    "the managed block here was edited outside suvadu — left in place so your edit isn't lost. Remove it by hand once you've saved what you need.".to_string(),
                ),
            });
            return Ok(());
        }
    }
    let updated = remove_codex_block(&existing, &block);
    if !dry_run {
        atomic_write_with_mode(path, &updated, 0o600)?;
    }
    report.push(FileChange {
        target: SyncTarget::Codex,
        path: path.to_path_buf(),
        kind: ChangeKind::Removed,
        skills: Vec::new(),
        diff: diff_lines(old_inner, ""),
        detail: Some("removed the managed block; the rest of the file is untouched.".to_string()),
    });
    Ok(())
}

// ── Diff ──────────────────────────────────────────────────────────────────

/// A minimal line diff: trim the common prefix/suffix and show what is left
/// as `-`/`+` lines. Enough to answer "exactly what will change in this
/// file?" without pulling in a diff crate. Trailing blank lines are
/// bookkeeping (the separator above suvadu's marker), not a change anyone
/// made, so they never appear as diff entries.
fn diff_lines(old: &str, new: &str) -> Vec<String> {
    let old_lines: Vec<&str> = old.trim_end_matches('\n').lines().collect();
    let new_lines: Vec<&str> = new.trim_end_matches('\n').lines().collect();

    let mut prefix = 0;
    while prefix < old_lines.len()
        && prefix < new_lines.len()
        && old_lines[prefix] == new_lines[prefix]
    {
        prefix += 1;
    }
    let mut suffix = 0;
    while suffix < old_lines.len() - prefix
        && suffix < new_lines.len() - prefix
        && old_lines[old_lines.len() - 1 - suffix] == new_lines[new_lines.len() - 1 - suffix]
    {
        suffix += 1;
    }

    let removed = &old_lines[prefix..old_lines.len() - suffix];
    let added = &new_lines[prefix..new_lines.len() - suffix];
    if removed.is_empty() && added.is_empty() {
        return Vec::new();
    }

    let total = removed.len() + added.len();
    let mut out: Vec<String> = removed
        .iter()
        .map(|l| format!("- {l}"))
        .chain(added.iter().map(|l| format!("+ {l}")))
        .take(MAX_DIFF_LINES)
        .collect();
    if total > MAX_DIFF_LINES {
        out.push(format!("… {} more changed line(s)", total - MAX_DIFF_LINES));
    }
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
    use crate::models::{NewSkill, SKILL_STATUS_PENDING};
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

    fn apply(
        repo: &Repository,
        targets: &[SyncTarget],
        cwd: &Path,
        home: Option<&Path>,
    ) -> SyncReport {
        sync_inner(repo, targets, cwd, home, None, SyncOptions::apply()).unwrap()
    }

    /// Change a skill's body so the next sync genuinely wants to rewrite
    /// its generated files.
    fn revise_body(repo: &Repository, name: &str, scope: &str) {
        repo.update_skill(
            name,
            scope,
            None,
            Some(&format!("Body for {name}, revised.")),
            None,
        )
        .unwrap()
        .expect("skill exists");
    }

    fn change_for<'a>(report: &'a SyncReport, path: &Path) -> &'a FileChange {
        report
            .changes
            .iter()
            .find(|c| c.path == path)
            .unwrap_or_else(|| panic!("no change reported for {}", path.display()))
    }

    #[test]
    fn sync_claude_code_writes_skill_md_with_frontmatter() {
        let (_dir, repo) = test_repo();
        repo.create_skill(&skill("deploy", SKILL_SCOPE_GLOBAL))
            .unwrap();

        let home = tempfile::tempdir().unwrap();
        let cwd = tempfile::tempdir().unwrap();
        let report = apply(
            &repo,
            &[SyncTarget::ClaudeCode],
            cwd.path(),
            Some(home.path()),
        );

        let content =
            std::fs::read_to_string(home.path().join(".claude/skills/deploy/SKILL.md")).unwrap();
        assert!(content.starts_with("---\nname: deploy\n"));
        assert!(content.contains("Body for deploy."));
        assert_eq!(report.written(), 1);
    }

    #[test]
    fn sync_is_idempotent_second_run_writes_nothing() {
        let (_dir, repo) = test_repo();
        repo.create_skill(&skill("deploy", SKILL_SCOPE_GLOBAL))
            .unwrap();
        let home = tempfile::tempdir().unwrap();
        let cwd = tempfile::tempdir().unwrap();

        let targets = [
            SyncTarget::ClaudeCode,
            SyncTarget::Cursor,
            SyncTarget::Codex,
        ];
        let first = apply(&repo, &targets, cwd.path(), Some(home.path()));
        assert!(first.written() >= 3);

        let second = apply(&repo, &targets, cwd.path(), Some(home.path()));
        assert_eq!(
            second.written(),
            0,
            "unchanged content should not be rewritten"
        );
        assert!(second
            .changes
            .iter()
            .all(|c| c.kind == ChangeKind::Unchanged));
    }

    #[test]
    fn sync_project_scoped_skill_only_written_for_matching_cwd() {
        let (_dir, repo) = test_repo();
        let project_dir = tempfile::tempdir().unwrap();
        repo.create_skill(&skill("local-fix", &project_dir.path().to_string_lossy()))
            .unwrap();
        let home = tempfile::tempdir().unwrap();

        let other_dir = tempfile::tempdir().unwrap();
        let report = apply(
            &repo,
            &[SyncTarget::ClaudeCode],
            other_dir.path(),
            Some(home.path()),
        );
        assert_eq!(report.written(), 0);

        let report = apply(
            &repo,
            &[SyncTarget::ClaudeCode],
            project_dir.path(),
            Some(home.path()),
        );
        assert_eq!(report.written(), 1);
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
            SyncOptions::preview(),
        )
        .unwrap();
        assert_eq!(report.written(), 1);
        assert!(!home
            .path()
            .join(".claude/skills/preview-me/SKILL.md")
            .exists());
    }

    #[test]
    fn dry_run_preview_names_the_exact_file_and_shows_the_managed_change() {
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
            SyncOptions::preview(),
        )
        .unwrap();

        let expected = home.path().join(".claude/skills/deploy/SKILL.md");
        let change = change_for(&report, &expected);
        assert_eq!(change.kind, ChangeKind::Create);
        assert_eq!(change.skills, vec!["deploy".to_string()]);
        assert!(change.diff.contains(&"+ Body for deploy.".to_string()));
        // The blank separator line above suvadu's own marker is bookkeeping;
        // it must not show up as a change. (An interior blank line, like the
        // one under the frontmatter, is real content and does.)
        assert_ne!(
            change.diff.last().map(String::as_str),
            Some("+ "),
            "trailing blank line leaked into the preview: {:?}",
            change.diff
        );

        let rendered = report.lines(true).join("\n");
        assert!(rendered.contains(&expected.display().to_string()));
        assert!(rendered.contains("would create"));
        assert!(rendered.contains("+ Body for deploy."));
    }

    #[test]
    fn preview_of_an_edited_skill_shows_only_the_changed_lines() {
        let (_dir, repo) = test_repo();
        repo.create_skill(&skill("deploy", SKILL_SCOPE_GLOBAL))
            .unwrap();
        let home = tempfile::tempdir().unwrap();
        let cwd = tempfile::tempdir().unwrap();
        apply(
            &repo,
            &[SyncTarget::ClaudeCode],
            cwd.path(),
            Some(home.path()),
        );

        repo.update_skill(
            "deploy",
            SKILL_SCOPE_GLOBAL,
            None,
            Some("Body for deploy, revised."),
            None,
        )
        .unwrap();

        let report = sync_inner(
            &repo,
            &[SyncTarget::ClaudeCode],
            cwd.path(),
            Some(home.path()),
            None,
            SyncOptions::preview(),
        )
        .unwrap();
        let change = change_for(&report, &home.path().join(".claude/skills/deploy/SKILL.md"));
        assert_eq!(change.kind, ChangeKind::Update);
        assert_eq!(
            change.diff,
            vec![
                "- Body for deploy.".to_string(),
                "+ Body for deploy, revised.".to_string()
            ]
        );
    }

    #[test]
    fn external_edit_of_a_generated_file_is_not_silently_overwritten() {
        let (_dir, repo) = test_repo();
        repo.create_skill(&skill("deploy", SKILL_SCOPE_GLOBAL))
            .unwrap();
        let home = tempfile::tempdir().unwrap();
        let cwd = tempfile::tempdir().unwrap();
        let targets = [SyncTarget::ClaudeCode];
        apply(&repo, &targets, cwd.path(), Some(home.path()));

        // The user hand-edits the generated file.
        let path = home.path().join(".claude/skills/deploy/SKILL.md");
        let edited = std::fs::read_to_string(&path)
            .unwrap()
            .replace("Body for deploy.", "MY OWN NOTES");
        std::fs::write(&path, &edited).unwrap();

        let report = apply(&repo, &targets, cwd.path(), Some(home.path()));
        let after = std::fs::read_to_string(&path).unwrap();
        assert!(
            after.contains("MY OWN NOTES"),
            "hand-edited content was silently overwritten: {after}"
        );
        assert_eq!(report.written(), 0, "a conflicted file must not be written");
        assert_eq!(report.conflicts(), 1);
        let change = change_for(&report, &path);
        assert_eq!(change.kind, ChangeKind::Conflict);
        assert!(change.detail.as_ref().unwrap().contains("--force"));
        // The conflict is actionable: it shows what suvadu would replace.
        assert!(change.diff.iter().any(|l| l.contains("MY OWN NOTES")));
    }

    #[test]
    fn force_overwrites_a_conflicted_file() {
        let (_dir, repo) = test_repo();
        repo.create_skill(&skill("deploy", SKILL_SCOPE_GLOBAL))
            .unwrap();
        let home = tempfile::tempdir().unwrap();
        let cwd = tempfile::tempdir().unwrap();
        let targets = [SyncTarget::ClaudeCode];
        apply(&repo, &targets, cwd.path(), Some(home.path()));
        let path = home.path().join(".claude/skills/deploy/SKILL.md");
        std::fs::write(&path, "hand written\n").unwrap();

        let report = sync_inner(
            &repo,
            &targets,
            cwd.path(),
            Some(home.path()),
            None,
            SyncOptions {
                dry_run: false,
                force: true,
            },
        )
        .unwrap();
        assert_eq!(report.conflicts(), 0);
        assert_eq!(report.written(), 1);
        assert!(std::fs::read_to_string(&path)
            .unwrap()
            .contains("Body for deploy."));
    }

    #[test]
    fn a_file_suvadu_never_wrote_is_a_conflict_not_an_overwrite() {
        let (_dir, repo) = test_repo();
        repo.create_skill(&skill("deploy", SKILL_SCOPE_GLOBAL))
            .unwrap();
        let home = tempfile::tempdir().unwrap();
        let cwd = tempfile::tempdir().unwrap();
        let path = home.path().join(".claude/skills/deploy/SKILL.md");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, "my own hand-written claude skill\n").unwrap();

        let report = apply(
            &repo,
            &[SyncTarget::ClaudeCode],
            cwd.path(),
            Some(home.path()),
        );
        assert_eq!(report.conflicts(), 1);
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "my own hand-written claude skill\n"
        );
    }

    #[test]
    fn a_pre_marker_generated_file_is_adopted_without_a_conflict() {
        let (_dir, repo) = test_repo();
        let created = repo
            .create_skill(&skill("legacy", SKILL_SCOPE_GLOBAL))
            .unwrap();
        let home = tempfile::tempdir().unwrap();
        let cwd = tempfile::tempdir().unwrap();
        let path = home.path().join(".claude/skills/legacy/SKILL.md");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        // Exactly what the pre-checksum generator produced.
        std::fs::write(&path, claude_base(&created)).unwrap();

        let report = apply(
            &repo,
            &[SyncTarget::ClaudeCode],
            cwd.path(),
            Some(home.path()),
        );
        assert_eq!(report.conflicts(), 0);
        assert_eq!(report.written(), 1, "the marker should be added");
        assert!(std::fs::read_to_string(&path)
            .unwrap()
            .contains(MANAGED_MARKER_PREFIX));
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
            SyncOptions::apply(),
        )
        .unwrap();
        let content = std::fs::read_to_string(codex_home.path().join("AGENTS.md")).unwrap();
        assert!(content.contains("Don't touch this."));
        assert!(content.contains(CODEX_BLOCK_START_PREFIX));
        assert!(content.contains("ci-tips"));

        // Re-running should replace only the managed block, not duplicate it.
        sync_inner(
            &repo,
            &[SyncTarget::Codex],
            cwd.path(),
            None,
            Some(codex_home.path()),
            SyncOptions::apply(),
        )
        .unwrap();
        let content2 = std::fs::read_to_string(codex_home.path().join("AGENTS.md")).unwrap();
        assert_eq!(content2.matches(CODEX_BLOCK_START_PREFIX).count(), 1);
        assert_eq!(content, content2);
    }

    #[test]
    fn codex_block_edited_by_hand_is_a_conflict_and_user_text_survives() {
        let (_dir, repo) = test_repo();
        repo.create_skill(&skill("ci-tips", SKILL_SCOPE_GLOBAL))
            .unwrap();
        let cwd = tempfile::tempdir().unwrap();
        let codex_home = tempfile::tempdir().unwrap();
        let path = codex_home.path().join("AGENTS.md");
        std::fs::write(&path, "# Mine\n\nKeep me.\n").unwrap();
        sync_inner(
            &repo,
            &[SyncTarget::Codex],
            cwd.path(),
            None,
            Some(codex_home.path()),
            SyncOptions::apply(),
        )
        .unwrap();

        let edited = std::fs::read_to_string(&path)
            .unwrap()
            .replace("Body for ci-tips.", "hand edited inside the block");
        std::fs::write(&path, &edited).unwrap();

        let report = sync_inner(
            &repo,
            &[SyncTarget::Codex],
            cwd.path(),
            None,
            Some(codex_home.path()),
            SyncOptions::apply(),
        )
        .unwrap();
        assert_eq!(report.conflicts(), 1);
        let after = std::fs::read_to_string(&path).unwrap();
        assert!(after.contains("hand edited inside the block"));
        assert!(after.contains("Keep me."));
    }

    #[test]
    fn codex_legacy_block_without_a_checksum_is_adopted() {
        let (_dir, repo) = test_repo();
        repo.create_skill(&skill("ci-tips", SKILL_SCOPE_GLOBAL))
            .unwrap();
        let cwd = tempfile::tempdir().unwrap();
        let codex_home = tempfile::tempdir().unwrap();
        let path = codex_home.path().join("AGENTS.md");
        std::fs::write(
            &path,
            format!("# Mine\n\n<!-- suvadu:skills:start -->\nold content\n{CODEX_BLOCK_END}\n"),
        )
        .unwrap();

        let report = sync_inner(
            &repo,
            &[SyncTarget::Codex],
            cwd.path(),
            None,
            Some(codex_home.path()),
            SyncOptions::apply(),
        )
        .unwrap();
        assert_eq!(report.conflicts(), 0);
        let after = std::fs::read_to_string(&path).unwrap();
        assert!(after.contains("# Mine"));
        assert!(after.contains("checksum="));
        assert!(!after.contains("old content"));
    }

    #[test]
    fn project_scope_shadows_global_of_the_same_name_and_says_so() {
        let (_dir, repo) = test_repo();
        let cwd = tempfile::tempdir().unwrap();
        let home = tempfile::tempdir().unwrap();
        repo.create_skill(&NewSkill {
            body: "global version".into(),
            ..skill("shared", SKILL_SCOPE_GLOBAL)
        })
        .unwrap();
        repo.create_skill(&NewSkill {
            body: "project version".into(),
            ..skill("shared", &cwd.path().to_string_lossy())
        })
        .unwrap();

        let report = apply(&repo, &[SyncTarget::Cursor], cwd.path(), Some(home.path()));
        assert_eq!(report.changes.len(), 1, "only one file for one name");
        assert!(report.notes.iter().any(|n| n.contains("shadows")));
        let content = std::fs::read_to_string(cwd.path().join(".cursor/rules/shared.mdc")).unwrap();
        assert!(content.contains("project version"));
        assert!(!content.contains("global version"));
    }

    #[test]
    fn pending_proposals_are_never_materialized() {
        let (_dir, repo) = test_repo();
        repo.create_skill(&NewSkill {
            source: "agent:claude-code".into(),
            status: SKILL_STATUS_PENDING.into(),
            ..skill("proposed", SKILL_SCOPE_GLOBAL)
        })
        .unwrap();
        let home = tempfile::tempdir().unwrap();
        let cwd = tempfile::tempdir().unwrap();

        let report = apply(&repo, &ALL_TARGETS, cwd.path(), Some(home.path()));
        assert_eq!(report.written(), 0);
        assert!(!home.path().join(".claude/skills/proposed").exists());
        assert_eq!(
            repo.get_skill("proposed", SKILL_SCOPE_GLOBAL)
                .unwrap()
                .unwrap()
                .status,
            SKILL_STATUS_PENDING,
            "a sync must never activate a proposal"
        );
    }

    #[test]
    fn destinations_name_every_target_file_for_an_active_skill() {
        let cwd = tempfile::tempdir().unwrap();
        let home = tempfile::tempdir().unwrap();
        let (_dir, repo) = test_repo();
        let s = repo
            .create_skill(&skill("deploy", SKILL_SCOPE_GLOBAL))
            .unwrap();

        let dests = destinations_inner(&s, &ALL_TARGETS, cwd.path(), Some(home.path()), None);
        let paths: Vec<String> = dests.iter().map(|d| d.path.display().to_string()).collect();
        assert!(paths
            .iter()
            .any(|p| p.ends_with(".claude/skills/deploy/SKILL.md")));
        assert!(paths
            .iter()
            .any(|p| p.ends_with(".cursor/rules/deploy.mdc")));
        assert!(paths.iter().any(|p| p.ends_with(".codex/AGENTS.md")));
        assert_eq!(
            dests
                .iter()
                .find(|d| d.target == SyncTarget::Codex)
                .unwrap()
                .managed,
            ManagedKind::Block
        );
    }

    #[test]
    fn destinations_are_empty_for_a_pending_or_out_of_scope_skill() {
        let cwd = tempfile::tempdir().unwrap();
        let other = tempfile::tempdir().unwrap();
        let home = tempfile::tempdir().unwrap();
        let (_dir, repo) = test_repo();
        let pending = repo
            .create_skill(&NewSkill {
                status: SKILL_STATUS_PENDING.into(),
                ..skill("proposed", SKILL_SCOPE_GLOBAL)
            })
            .unwrap();
        assert!(
            destinations_inner(&pending, &ALL_TARGETS, cwd.path(), Some(home.path()), None)
                .is_empty()
        );

        let elsewhere = repo
            .create_skill(&skill("elsewhere", &other.path().to_string_lossy()))
            .unwrap();
        assert!(destinations_inner(
            &elsewhere,
            &ALL_TARGETS,
            cwd.path(),
            Some(home.path()),
            None
        )
        .is_empty());
    }

    // ── Cleanup ───────────────────────────────────────────────────────────

    #[test]
    fn cleanup_removes_files_left_behind_by_a_removed_skill() {
        let (_dir, repo) = test_repo();
        repo.create_skill(&skill("gone", SKILL_SCOPE_GLOBAL))
            .unwrap();
        let home = tempfile::tempdir().unwrap();
        let cwd = tempfile::tempdir().unwrap();
        let codex_home = tempfile::tempdir().unwrap();
        sync_inner(
            &repo,
            &ALL_TARGETS,
            cwd.path(),
            Some(home.path()),
            Some(codex_home.path()),
            SyncOptions::apply(),
        )
        .unwrap();
        let claude = home.path().join(".claude/skills/gone/SKILL.md");
        let cursor = cwd.path().join(".cursor/rules/gone.mdc");
        assert!(claude.exists() && cursor.exists());

        // Removing the skill from the library leaves the files behind…
        repo.delete_skill("gone", SKILL_SCOPE_GLOBAL).unwrap();
        assert!(claude.exists(), "rm must not touch generated files");

        // …until cleanup is asked for explicitly. Dry run first.
        let preview = cleanup_inner(
            &repo,
            &ALL_TARGETS,
            cwd.path(),
            Some(home.path()),
            Some(codex_home.path()),
            true,
        )
        .unwrap();
        assert_eq!(preview.removed(), 3);
        assert!(claude.exists(), "a dry run must not remove anything");

        let report = cleanup_inner(
            &repo,
            &ALL_TARGETS,
            cwd.path(),
            Some(home.path()),
            Some(codex_home.path()),
            false,
        )
        .unwrap();
        assert_eq!(report.removed(), 3);
        assert!(!claude.exists());
        assert!(!cursor.exists());
        let agents = std::fs::read_to_string(codex_home.path().join("AGENTS.md")).unwrap();
        assert!(!agents.contains(CODEX_BLOCK_START_PREFIX));
    }

    #[test]
    fn cleanup_keeps_files_for_skills_that_are_still_active() {
        let (_dir, repo) = test_repo();
        repo.create_skill(&skill("keep", SKILL_SCOPE_GLOBAL))
            .unwrap();
        repo.create_skill(&skill("drop", SKILL_SCOPE_GLOBAL))
            .unwrap();
        let home = tempfile::tempdir().unwrap();
        let cwd = tempfile::tempdir().unwrap();
        apply(
            &repo,
            &[SyncTarget::ClaudeCode],
            cwd.path(),
            Some(home.path()),
        );
        repo.delete_skill("drop", SKILL_SCOPE_GLOBAL).unwrap();

        let report = cleanup_inner(
            &repo,
            &[SyncTarget::ClaudeCode],
            cwd.path(),
            Some(home.path()),
            None,
            false,
        )
        .unwrap();
        assert_eq!(report.removed(), 1);
        assert!(home.path().join(".claude/skills/keep/SKILL.md").exists());
        assert!(!home.path().join(".claude/skills/drop/SKILL.md").exists());
    }

    #[test]
    fn cleanup_removes_files_for_a_disabled_skill_too() {
        let (_dir, repo) = test_repo();
        repo.create_skill(&skill("paused", SKILL_SCOPE_GLOBAL))
            .unwrap();
        let home = tempfile::tempdir().unwrap();
        let cwd = tempfile::tempdir().unwrap();
        apply(
            &repo,
            &[SyncTarget::ClaudeCode],
            cwd.path(),
            Some(home.path()),
        );
        repo.set_skill_status(
            "paused",
            SKILL_SCOPE_GLOBAL,
            crate::models::SKILL_STATUS_ARCHIVED,
        )
        .unwrap();

        let report = cleanup_inner(
            &repo,
            &[SyncTarget::ClaudeCode],
            cwd.path(),
            Some(home.path()),
            None,
            false,
        )
        .unwrap();
        assert_eq!(report.removed(), 1);
        assert!(!home.path().join(".claude/skills/paused/SKILL.md").exists());
    }

    #[test]
    fn cleanup_never_deletes_a_file_suvadu_did_not_write() {
        let (_dir, repo) = test_repo();
        let home = tempfile::tempdir().unwrap();
        let cwd = tempfile::tempdir().unwrap();
        let mine = home.path().join(".claude/skills/handmade/SKILL.md");
        std::fs::create_dir_all(mine.parent().unwrap()).unwrap();
        std::fs::write(&mine, "mine, not suvadu's\n").unwrap();

        let report = cleanup_inner(
            &repo,
            &[SyncTarget::ClaudeCode],
            cwd.path(),
            Some(home.path()),
            None,
            false,
        )
        .unwrap();
        assert_eq!(report.removed(), 0);
        assert!(mine.exists());
        let change = change_for(&report, &mine);
        assert_eq!(change.kind, ChangeKind::Skipped);
        assert!(change.detail.as_ref().unwrap().contains("by hand"));
    }

    #[test]
    fn cleanup_leaves_unrelated_agents_md_content_alone() {
        let (_dir, repo) = test_repo();
        repo.create_skill(&skill("temp", SKILL_SCOPE_GLOBAL))
            .unwrap();
        let cwd = tempfile::tempdir().unwrap();
        let codex_home = tempfile::tempdir().unwrap();
        let path = codex_home.path().join("AGENTS.md");
        std::fs::write(&path, "# House rules\n\nAlways run the tests.\n").unwrap();
        sync_inner(
            &repo,
            &[SyncTarget::Codex],
            cwd.path(),
            None,
            Some(codex_home.path()),
            SyncOptions::apply(),
        )
        .unwrap();
        repo.delete_skill("temp", SKILL_SCOPE_GLOBAL).unwrap();

        cleanup_inner(
            &repo,
            &[SyncTarget::Codex],
            cwd.path(),
            None,
            Some(codex_home.path()),
            false,
        )
        .unwrap();
        let after = std::fs::read_to_string(&path).unwrap();
        assert_eq!(after, "# House rules\n\nAlways run the tests.\n");
    }

    #[test]
    fn cleanup_leaves_a_hand_edited_managed_block_in_place() {
        let (_dir, repo) = test_repo();
        repo.create_skill(&skill("temp", SKILL_SCOPE_GLOBAL))
            .unwrap();
        let cwd = tempfile::tempdir().unwrap();
        let codex_home = tempfile::tempdir().unwrap();
        let path = codex_home.path().join("AGENTS.md");
        sync_inner(
            &repo,
            &[SyncTarget::Codex],
            cwd.path(),
            None,
            Some(codex_home.path()),
            SyncOptions::apply(),
        )
        .unwrap();
        let edited = std::fs::read_to_string(&path)
            .unwrap()
            .replace("Body for temp.", "my own addition");
        std::fs::write(&path, edited).unwrap();
        repo.delete_skill("temp", SKILL_SCOPE_GLOBAL).unwrap();

        let report = cleanup_inner(
            &repo,
            &[SyncTarget::Codex],
            cwd.path(),
            None,
            Some(codex_home.path()),
            false,
        )
        .unwrap();
        assert_eq!(report.removed(), 0);
        assert!(std::fs::read_to_string(&path)
            .unwrap()
            .contains("my own addition"));
        assert_eq!(report.changes[0].kind, ChangeKind::Skipped);
    }

    // ── Helpers ───────────────────────────────────────────────────────────

    #[test]
    fn diff_lines_reports_only_the_changed_lines() {
        let out = diff_lines("a\nb\nc\n", "a\nB\nc\n");
        assert_eq!(out, vec!["- b".to_string(), "+ B".to_string()]);
    }

    #[test]
    fn diff_lines_is_empty_for_identical_content() {
        assert!(diff_lines("same\n", "same\n").is_empty());
    }

    #[test]
    fn diff_lines_truncates_a_huge_change() {
        let mut big = String::new();
        for i in 0..200 {
            let _ = writeln!(big, "line {i}");
        }
        let out = diff_lines("", &big);
        assert_eq!(out.len(), MAX_DIFF_LINES + 1);
        assert!(out.last().unwrap().contains("more changed line(s)"));
    }

    #[test]
    fn split_managed_reads_back_the_checksum_it_wrote() {
        let content = managed_file_content("hello\n", "demo");
        let (before, sum, after) = split_managed(&content).unwrap();
        assert_eq!(before, "hello\n\n");
        assert_eq!(sum, short_checksum(before));
        assert_eq!(after, "", "suvadu's marker is the last line it writes");
        assert!(content.contains("skill=demo"));
    }

    #[test]
    fn split_managed_reports_content_written_below_the_marker() {
        let content = managed_file_content("hello\n", "demo") + "mine\n";
        let (_, _, after) = split_managed(&content).unwrap();
        assert_eq!(after, "mine\n");
        assert!(managed_region(&content).contains("mine"));
    }

    #[test]
    fn classify_file_distinguishes_ours_drifted_and_foreign() {
        let base = "body\n";
        let ours = managed_file_content(base, "demo");
        assert_eq!(classify_file(None, base), Ownership::Absent);
        assert_eq!(classify_file(Some(&ours), base), Ownership::Ours);
        assert_eq!(
            classify_file(Some(&ours.replace("body", "edited")), base),
            Ownership::Drifted
        );
        assert_eq!(classify_file(Some(base), base), Ownership::Ours);
        assert_eq!(
            classify_file(Some("someone else\n"), base),
            Ownership::Foreign
        );
        assert_eq!(
            classify_file(Some(&format!("{ours}my own notes\n")), base),
            Ownership::Appended,
            "text below the marker is not suvadu's to delete"
        );
        assert_eq!(
            classify_file(Some(&format!("{ours}\n\n")), base),
            Ownership::Ours,
            "trailing blank lines are not user content"
        );
    }

    #[test]
    fn append_block_keeps_existing_content() {
        let out = append_block("existing content\n", "<start>new<end>");
        assert!(out.starts_with("existing content\n"));
        assert!(out.contains("<start>new<end>"));
    }

    // ── Content written below suvadu's marker (whole-file targets) ────────
    //
    // The marker is *terminal*: it is the last thing suvadu writes, so
    // anything after it came from somebody else. These cover appended text
    // for preview, apply, --force and cleanup, on both whole-file targets.

    const APPENDED: &str = "\nUSER APPENDED INSTRUCTIONS\n";

    /// Sync `name` into both whole-file targets, then append user text to
    /// the file at `path`. Returns the full file content afterwards.
    fn sync_then_append(
        repo: &Repository,
        targets: &[SyncTarget],
        cwd: &Path,
        home: Option<&Path>,
        path: &Path,
    ) -> String {
        apply(repo, targets, cwd, home);
        let appended = std::fs::read_to_string(path).unwrap() + APPENDED;
        std::fs::write(path, &appended).unwrap();
        appended
    }

    #[test]
    fn text_appended_below_the_claude_marker_is_a_conflict_not_a_silent_deletion() {
        let (_dir, repo) = test_repo();
        repo.create_skill(&skill("alpha", SKILL_SCOPE_GLOBAL))
            .unwrap();
        let home = tempfile::tempdir().unwrap();
        let cwd = tempfile::tempdir().unwrap();
        let targets = [SyncTarget::ClaudeCode];
        let path = home.path().join(".claude/skills/alpha/SKILL.md");
        let appended = sync_then_append(&repo, &targets, cwd.path(), Some(home.path()), &path);

        // Change the skill so a sync genuinely wants to rewrite the file.
        revise_body(&repo, "alpha", SKILL_SCOPE_GLOBAL);

        let report = apply(&repo, &targets, cwd.path(), Some(home.path()));
        assert_eq!(report.written(), 0, "a write here destroys the user's text");
        assert_eq!(report.conflicts(), 1);
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            appended,
            "appended user instructions were silently deleted"
        );
        let change = change_for(&report, &path);
        assert_eq!(change.kind, ChangeKind::Conflict);
        assert!(change.detail.as_ref().unwrap().contains("--force"));
    }

    #[test]
    fn preview_shows_appended_text_as_a_removal_instead_of_a_clean_update() {
        let (_dir, repo) = test_repo();
        repo.create_skill(&skill("alpha", SKILL_SCOPE_GLOBAL))
            .unwrap();
        let home = tempfile::tempdir().unwrap();
        let cwd = tempfile::tempdir().unwrap();
        let targets = [SyncTarget::ClaudeCode];
        let path = home.path().join(".claude/skills/alpha/SKILL.md");
        sync_then_append(&repo, &targets, cwd.path(), Some(home.path()), &path);

        let preview = sync_inner(
            &repo,
            &targets,
            cwd.path(),
            Some(home.path()),
            None,
            SyncOptions::preview(),
        )
        .unwrap();
        let change = change_for(&preview, &path);
        assert_eq!(
            change.kind,
            ChangeKind::Conflict,
            "a preview must not promise a clean 'would update'"
        );
        assert!(
            change
                .diff
                .iter()
                .any(|l| l.starts_with('-') && l.contains("USER APPENDED INSTRUCTIONS")),
            "the preview must show the appended text as removed: {:?}",
            change.diff
        );
        // The rendered preview shows a conflict's diff even without --diff.
        let rendered = preview.lines(false).join("\n");
        assert!(
            rendered.contains("USER APPENDED INSTRUCTIONS"),
            "{rendered}"
        );
    }

    #[test]
    fn text_appended_below_a_cursor_rule_marker_is_a_conflict() {
        let (_dir, repo) = test_repo();
        repo.create_skill(&skill("alpha", SKILL_SCOPE_GLOBAL))
            .unwrap();
        let home = tempfile::tempdir().unwrap();
        let cwd = tempfile::tempdir().unwrap();
        let targets = [SyncTarget::Cursor];
        let path = cursor_rule_path(cwd.path(), "alpha");
        let appended = sync_then_append(&repo, &targets, cwd.path(), Some(home.path()), &path);
        revise_body(&repo, "alpha", SKILL_SCOPE_GLOBAL);

        let report = apply(&repo, &targets, cwd.path(), Some(home.path()));
        assert_eq!(report.conflicts(), 1);
        assert_eq!(report.written(), 0);
        assert_eq!(std::fs::read_to_string(&path).unwrap(), appended);
    }

    #[test]
    fn text_prepended_above_the_marker_is_a_conflict() {
        let (_dir, repo) = test_repo();
        repo.create_skill(&skill("alpha", SKILL_SCOPE_GLOBAL))
            .unwrap();
        let home = tempfile::tempdir().unwrap();
        let cwd = tempfile::tempdir().unwrap();
        let targets = [SyncTarget::ClaudeCode];
        apply(&repo, &targets, cwd.path(), Some(home.path()));
        let path = home.path().join(".claude/skills/alpha/SKILL.md");
        let prepended = format!(
            "USER PREPENDED INSTRUCTIONS\n{}",
            std::fs::read_to_string(&path).unwrap()
        );
        std::fs::write(&path, &prepended).unwrap();

        let report = apply(&repo, &targets, cwd.path(), Some(home.path()));
        assert_eq!(report.conflicts(), 1);
        assert_eq!(std::fs::read_to_string(&path).unwrap(), prepended);
    }

    #[test]
    fn force_is_the_only_way_appended_text_is_overwritten() {
        let (_dir, repo) = test_repo();
        repo.create_skill(&skill("alpha", SKILL_SCOPE_GLOBAL))
            .unwrap();
        let home = tempfile::tempdir().unwrap();
        let cwd = tempfile::tempdir().unwrap();
        let targets = [SyncTarget::ClaudeCode];
        let path = home.path().join(".claude/skills/alpha/SKILL.md");
        sync_then_append(&repo, &targets, cwd.path(), Some(home.path()), &path);

        let report = sync_inner(
            &repo,
            &targets,
            cwd.path(),
            Some(home.path()),
            None,
            SyncOptions {
                dry_run: false,
                force: true,
            },
        )
        .unwrap();
        assert_eq!(report.conflicts(), 0);
        assert_eq!(report.written(), 1);
        let after = std::fs::read_to_string(&path).unwrap();
        assert!(!after.contains("USER APPENDED INSTRUCTIONS"));
    }

    #[test]
    fn codex_keeps_hand_written_text_below_its_managed_block() {
        let (_dir, repo) = test_repo();
        repo.create_skill(&skill("alpha", SKILL_SCOPE_GLOBAL))
            .unwrap();
        let cwd = tempfile::tempdir().unwrap();
        let codex_home = tempfile::tempdir().unwrap();
        let path = codex_home.path().join("AGENTS.md");
        let targets = [SyncTarget::Codex];
        sync_inner(
            &repo,
            &targets,
            cwd.path(),
            None,
            Some(codex_home.path()),
            SyncOptions::apply(),
        )
        .unwrap();
        std::fs::write(
            &path,
            std::fs::read_to_string(&path).unwrap() + "\nUSER APPENDED INSTRUCTIONS\n",
        )
        .unwrap();
        revise_body(&repo, "alpha", SKILL_SCOPE_GLOBAL);

        let report = sync_inner(
            &repo,
            &targets,
            cwd.path(),
            None,
            Some(codex_home.path()),
            SyncOptions::apply(),
        )
        .unwrap();
        assert_eq!(report.conflicts(), 0, "text outside the block is not drift");
        let after = std::fs::read_to_string(&path).unwrap();
        assert!(
            after.contains("USER APPENDED INSTRUCTIONS"),
            "text below the managed block must survive: {after}"
        );
        assert!(after.contains("Body for alpha, revised."));
    }

    #[test]
    fn cleanup_leaves_a_generated_file_with_appended_text_in_place() {
        let (_dir, repo) = test_repo();
        repo.create_skill(&skill("alpha", SKILL_SCOPE_GLOBAL))
            .unwrap();
        let home = tempfile::tempdir().unwrap();
        let cwd = tempfile::tempdir().unwrap();
        let targets = [SyncTarget::ClaudeCode, SyncTarget::Cursor];
        let claude = home.path().join(".claude/skills/alpha/SKILL.md");
        let cursor = cursor_rule_path(cwd.path(), "alpha");
        let claude_text = sync_then_append(&repo, &targets, cwd.path(), Some(home.path()), &claude);
        let cursor_text = std::fs::read_to_string(&cursor).unwrap() + APPENDED;
        std::fs::write(&cursor, &cursor_text).unwrap();

        repo.delete_skill("alpha", SKILL_SCOPE_GLOBAL).unwrap();
        let report =
            cleanup_inner(&repo, &targets, cwd.path(), Some(home.path()), None, false).unwrap();

        assert_eq!(report.removed(), 0, "deleting these loses the user's text");
        assert_eq!(std::fs::read_to_string(&claude).unwrap(), claude_text);
        assert_eq!(std::fs::read_to_string(&cursor).unwrap(), cursor_text);
        for path in [&claude, &cursor] {
            let change = change_for(&report, path);
            assert_eq!(change.kind, ChangeKind::Skipped);
            assert!(change.detail.is_some());
        }
    }

    // ── A project override must not uninstall the global skill ────────────

    /// Global `alpha` synced everywhere, then shadowed by a project-scoped
    /// `alpha` in `cwd` and synced again.
    fn shadowed_global_fixture(
        repo: &Repository,
        cwd: &Path,
        home: &Path,
        codex_home: &Path,
    ) -> Result<(), Box<dyn std::error::Error>> {
        repo.create_skill(&skill("alpha", SKILL_SCOPE_GLOBAL))?;
        sync_inner(
            repo,
            &ALL_TARGETS,
            cwd,
            Some(home),
            Some(codex_home),
            SyncOptions::apply(),
        )?;
        repo.create_skill(&skill("alpha", &cwd.to_string_lossy()))?;
        sync_inner(
            repo,
            &ALL_TARGETS,
            cwd,
            Some(home),
            Some(codex_home),
            SyncOptions::apply(),
        )?;
        Ok(())
    }

    #[test]
    fn cleanup_from_the_overriding_project_keeps_the_global_skill_installed() {
        let (_dir, repo) = test_repo();
        let home = tempfile::tempdir().unwrap();
        let cwd = tempfile::tempdir().unwrap();
        let codex_home = tempfile::tempdir().unwrap();
        shadowed_global_fixture(&repo, cwd.path(), home.path(), codex_home.path()).unwrap();

        let global_claude = home.path().join(".claude/skills/alpha/SKILL.md");
        let global_codex = codex_home.path().join("AGENTS.md");
        assert!(global_claude.exists());
        assert!(std::fs::read_to_string(&global_codex)
            .unwrap()
            .contains("Body for alpha."));

        let report = cleanup_inner(
            &repo,
            &ALL_TARGETS,
            cwd.path(),
            Some(home.path()),
            Some(codex_home.path()),
            false,
        )
        .unwrap();

        assert_eq!(
            report.removed(),
            0,
            "a project override must not uninstall the global skill for other projects: {:?}",
            report.changes
        );
        assert!(
            global_claude.exists(),
            "the active global Claude skill was deleted by a project cleanup"
        );
        let codex = std::fs::read_to_string(&global_codex).unwrap();
        assert!(
            codex.contains(CODEX_BLOCK_START_PREFIX) && codex.contains("Body for alpha."),
            "the active global Codex block was removed by a project cleanup: {codex}"
        );
    }

    #[test]
    fn cleanup_from_an_unrelated_directory_keeps_the_shadowed_global_skill() {
        let (_dir, repo) = test_repo();
        let home = tempfile::tempdir().unwrap();
        let project = tempfile::tempdir().unwrap();
        let codex_home = tempfile::tempdir().unwrap();
        shadowed_global_fixture(&repo, project.path(), home.path(), codex_home.path()).unwrap();

        let elsewhere = tempfile::tempdir().unwrap();
        let report = cleanup_inner(
            &repo,
            &ALL_TARGETS,
            elsewhere.path(),
            Some(home.path()),
            Some(codex_home.path()),
            false,
        )
        .unwrap();

        assert_eq!(report.removed(), 0, "{:?}", report.changes);
        assert!(home.path().join(".claude/skills/alpha/SKILL.md").exists());
        assert!(std::fs::read_to_string(codex_home.path().join("AGENTS.md"))
            .unwrap()
            .contains("Body for alpha."));
        // The project's own generated files are untouched from elsewhere.
        assert!(project
            .path()
            .join(".claude/skills/alpha/SKILL.md")
            .exists());
    }

    #[test]
    fn cleanup_still_removes_a_global_file_once_the_global_skill_is_gone() {
        let (_dir, repo) = test_repo();
        let home = tempfile::tempdir().unwrap();
        let cwd = tempfile::tempdir().unwrap();
        let codex_home = tempfile::tempdir().unwrap();
        shadowed_global_fixture(&repo, cwd.path(), home.path(), codex_home.path()).unwrap();

        repo.delete_skill("alpha", SKILL_SCOPE_GLOBAL).unwrap();
        let report = cleanup_inner(
            &repo,
            &ALL_TARGETS,
            cwd.path(),
            Some(home.path()),
            Some(codex_home.path()),
            false,
        )
        .unwrap();

        assert!(!home.path().join(".claude/skills/alpha/SKILL.md").exists());
        assert!(
            !std::fs::read_to_string(codex_home.path().join("AGENTS.md"))
                .unwrap()
                .contains(CODEX_BLOCK_START_PREFIX)
        );
        assert_eq!(report.removed(), 2);
        // The project-scoped skill of the same name keeps its own files.
        assert!(cwd.path().join(".claude/skills/alpha/SKILL.md").exists());
        assert!(cursor_rule_path(cwd.path(), "alpha").exists());
    }
}
