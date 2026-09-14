//! Skills suvadu itself seeds and keeps current in the shared `skills`
//! table (`SKILL_SOURCE_SUVADU`), distinct from human-authored
//! (`SKILL_SOURCE_HUMAN`) and agent-proposed (`agent:<name>`) skills. See
//! `docs/superpowers/specs/2026-09-14-session-memory-skill-design.md`.
use crate::config::Config;
use crate::db::DbResult;
use crate::models::{NewSkill, SKILL_SCOPE_GLOBAL, SKILL_SOURCE_SUVADU, SKILL_STATUS_ACTIVE};
use crate::repository::Repository;

#[allow(dead_code)]
const SESSION_MEMORY_DESCRIPTION: &str = "Use when the user asks to summarize, save, or recall what happened in the current or a recent coding/terminal session — prefer this over writing a generic memory note.";

#[allow(dead_code)]
const SESSION_MEMORY_BODY: &str =
    "When the user asks to summarize, save, or recall what happened in this
(or a recent) coding session — e.g. \"summarize this session and save it\",
\"what did we do earlier\" — prefer Suvadu's own session tracking over
writing a generic memory note:

1. Call `resolve_current_agent_session` (don't guess an ID).
2. Read it with `get_agent_session`, paging through events/commands.
3. If the user explicitly asked to save, call `save_session_summary` with
   the exact event/command IDs you cited as evidence.

Why: Suvadu's summary is anchored to the real commands and events
captured for this project, not a paraphrase — durable and re-readable by
any MCP client. Reserve a generic memory note for durable facts about the
user or project that should surface in *unrelated* future conversations
(preferences, ongoing initiatives) — not for narrating a specific
session's work.";

/// Suvadu-owned skills to seed/refresh, each gated on the config it
/// depends on. Currently just the one; a `Vec` so a future addition
/// doesn't need to change `ensure_installed`'s shape.
#[allow(dead_code)]
fn builtin_skills(config: &Config) -> Vec<NewSkill> {
    let mut skills = Vec::new();
    if config.mcp.allow_session_summaries {
        skills.push(NewSkill {
            name: "suvadu-session-memory".to_string(),
            description: SESSION_MEMORY_DESCRIPTION.to_string(),
            body: SESSION_MEMORY_BODY.to_string(),
            triggers: vec![],
            scope: SKILL_SCOPE_GLOBAL.to_string(),
            source: SKILL_SOURCE_SUVADU.to_string(),
            status: SKILL_STATUS_ACTIVE.to_string(),
        });
    }
    skills
}

/// Seed/refresh every suvadu-owned builtin skill whose gate is currently
/// satisfied. A skill whose gate is `false` (e.g.
/// `mcp.allow_session_summaries` off) is simply never created or updated
/// here — it is not retroactively removed if it already exists from a
/// time the gate was `true`, matching `skills_sync`'s existing behavior
/// for any skill that becomes inactive (no prune-on-deactivate path
/// exists for any skill today).
///
/// Returns `true` if anything was created or changed, so a caller that
/// wants to materialize immediately (`suv init claude-code`) knows
/// whether a sync is worth running at all.
#[allow(dead_code)]
pub fn ensure_installed(repo: &Repository, config: &Config) -> DbResult<bool> {
    let mut changed = false;
    for skill in builtin_skills(config) {
        let before_version = repo
            .get_skill(&skill.name, &skill.scope)?
            .map(|s| s.version);
        let after = repo.upsert_skill(&skill)?;
        if before_version != Some(after.version) {
            changed = true;
        }
    }
    Ok(changed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;
    use crate::test_utils::test_repo;

    fn config_with_summaries(enabled: bool) -> Config {
        let mut config = Config::default();
        config.mcp.allow_session_summaries = enabled;
        config
    }

    #[test]
    fn ensure_installed_skips_when_session_summaries_disabled() {
        let (_dir, repo) = test_repo();
        let changed = ensure_installed(&repo, &config_with_summaries(false)).unwrap();
        assert!(!changed);
        assert!(repo
            .get_skill("suvadu-session-memory", crate::models::SKILL_SCOPE_GLOBAL)
            .unwrap()
            .is_none());
    }

    #[test]
    fn ensure_installed_creates_when_enabled() {
        let (_dir, repo) = test_repo();
        let changed = ensure_installed(&repo, &config_with_summaries(true)).unwrap();
        assert!(changed);
        let skill = repo
            .get_skill("suvadu-session-memory", crate::models::SKILL_SCOPE_GLOBAL)
            .unwrap()
            .unwrap();
        assert_eq!(skill.source, crate::models::SKILL_SOURCE_SUVADU);
        assert_eq!(skill.status, crate::models::SKILL_STATUS_ACTIVE);
        assert!(skill.body.contains("resolve_current_agent_session"));
        assert!(skill.body.contains("save_session_summary"));
    }

    #[test]
    fn ensure_installed_is_idempotent_on_second_call() {
        let (_dir, repo) = test_repo();
        ensure_installed(&repo, &config_with_summaries(true)).unwrap();
        let changed_again = ensure_installed(&repo, &config_with_summaries(true)).unwrap();
        assert!(!changed_again);
    }
}
