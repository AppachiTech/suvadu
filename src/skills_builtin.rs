//! Skills suvadu itself seeds and keeps current in the shared `skills`
//! table (`SKILL_SOURCE_SUVADU`), distinct from human-authored
//! (`SKILL_SOURCE_HUMAN`) and agent-proposed (`agent:<name>`) skills. See
//! `docs/superpowers/specs/2026-09-14-session-memory-skill-design.md`.
use crate::config::Config;
use crate::db::DbResult;
use crate::models::{NewSkill, SKILL_SCOPE_GLOBAL, SKILL_SOURCE_SUVADU, SKILL_STATUS_ACTIVE};
use crate::repository::Repository;

const SESSION_MEMORY_DESCRIPTION: &str = "Use when the user asks to summarize, save, or recall what happened in the current or a recent coding/terminal session — prefer this over writing a generic memory note.";

/// Body of the session-memory skill. Built rather than stored as a literal
/// so the handoff section it teaches is generated from
/// [`crate::ai_sessions::handoff::SECTIONS`] — the same list the TUI's
/// handoff panel and the `prepare_session_handoff` MCP prompt use, so an
/// agent is never taught a different shape of handoff than suvadu produces.
fn session_memory_body() -> String {
    use std::fmt::Write;
    let mut body = String::from(
        "When the user asks to summarize, save, or recall what happened in this
(or a recent) coding session \u{2014} e.g. \"summarize this session and save it\",
\"what did we do earlier\" \u{2014} prefer Suvadu's own session tracking over
writing a generic memory note, if Suvadu's MCP tools are available in this
session:

1. Call `resolve_current_agent_session` (don't guess an ID). If it returns
   resolved=false or several candidates, ask the user which session they
   mean \u{2014} never pick the most recent one just because it is convenient.
2. Read it with `get_agent_session`, paging through events/commands until
   next_event_offset and next_command_offset are both null. Repeat the
   session's `capture.known_missing` entries rather than implying the
   capture is complete: a captured final answer does not prove every
   command was captured.
3. Check the newest saved summary's basis. CURRENT means don't duplicate
   it; NEW ACTIVITY means extend it from its source counts and pass it as
   base_summary_id; EVIDENCE CHANGED means rebuild from the whole session.
4. If \u{2014} and only if \u{2014} the user explicitly asked to save, call
   `save_session_summary` with the exact event/command IDs you cited as
   evidence. Suvadu never generates the text and never calls a model: you
   author it, and the user asks for it.

If these tools aren't available (Suvadu's MCP server isn't connected in
this session), fall back to a normal memory note instead. If
`save_session_summary` is missing or refuses because writes are off, tell
the user to enable it in `suv settings` \u{2192} MCP \u{2192} Writes \u{2192} Allow Saved
Session Summaries and restart this client; do not retry in a loop.

## Handing off to another agent

When the user wants to hand the session to a different agent, use the
`prepare_session_handoff` prompt, or write these sections yourself:

",
    );
    for (name, guidance) in crate::ai_sessions::handoff::SECTIONS {
        let _ = writeln!(body, "- {name}: {guidance}");
    }
    body.push_str(
        "
Cite the exact event or command ID behind every factual line, and write
\"not captured\" rather than guessing \u{2014} Suvadu records no file contents and
no command output, so a command that claims to change files is a claim to
check, not an observed diff.

Why: Suvadu's summary is anchored to the real commands and events
captured for this project, not a paraphrase \u{2014} durable and re-readable by
any MCP client. Reserve a generic memory note for durable facts about the
user or project that should surface in *unrelated* future conversations
(preferences, ongoing initiatives) \u{2014} not for narrating a specific
session's work.",
    );
    body
}

/// Suvadu-owned skills to seed/refresh, each gated on the config it
/// depends on. Currently just the one; a `Vec` so a future addition
/// doesn't need to change `ensure_installed`'s shape.
fn builtin_skills(config: &Config) -> Vec<NewSkill> {
    let mut skills = Vec::new();
    if config.mcp.allow_session_summaries {
        skills.push(NewSkill {
            name: "suvadu-session-memory".to_string(),
            description: SESSION_MEMORY_DESCRIPTION.to_string(),
            body: session_memory_body(),
            triggers: vec![],
            scope: SKILL_SCOPE_GLOBAL.to_string(),
            source: SKILL_SOURCE_SUVADU.to_string(),
            status: SKILL_STATUS_ACTIVE.to_string(),
        });
    }
    skills
}

/// Whether any suvadu-owned builtin skill's gate is currently satisfied — i.e.
/// whether `ensure_installed` would seed or keep anything active for this
/// config. Lets a caller that wants unconditional materialization (not just
/// on change, see `ensure_installed`'s doc comment) decide whether running a
/// sync is worth it at all, without duplicating each skill's own gate
/// condition here.
pub fn any_builtin_skill_enabled(config: &Config) -> bool {
    !builtin_skills(config).is_empty()
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

    /// The skill, the MCP prompt and the TUI panel must describe one
    /// handoff. Generating the list here from `handoff::SECTIONS` is what
    /// stops the skill quietly teaching an older shape.
    #[test]
    fn session_memory_body_teaches_every_handoff_section() {
        let body = session_memory_body();
        for (name, _) in crate::ai_sessions::handoff::SECTIONS {
            assert!(body.contains(name), "skill body is missing {name}");
        }
        assert!(body.contains("prepare_session_handoff"));
    }

    /// The write policy is the product promise: the user asks, the
    /// connected agent authors, suvadu never calls a model.
    #[test]
    fn session_memory_body_states_the_explicit_request_and_author_policy() {
        let body = session_memory_body();
        assert!(body.contains("only if \u{2014} the user explicitly asked to save"));
        assert!(body.contains("never generates the text and never calls a model"));
        assert!(body.contains("never pick the most recent one"));
        assert!(body.contains("Allow Saved\nSession Summaries"));
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

    #[test]
    fn any_builtin_skill_enabled_matches_the_gate() {
        assert!(!any_builtin_skill_enabled(&config_with_summaries(false)));
        assert!(any_builtin_skill_enabled(&config_with_summaries(true)));
    }
}
