//! Cursor's agent hooks as a session: the prompt (`beforeSubmitPrompt`),
//! the final response (`afterAgentResponse`) and the turn end (`stop`),
//! each pushed by Cursor as it happens and stored through
//! [`crate::ai_sessions::cursor`]. Shell commands come separately, through
//! `afterShellExecution`, under the same `cursor-<conversation_id>` session.
//!
//! None of these hooks may hold Cursor up: a capture that fails is
//! reported on stderr and the hook still succeeds, and the prompt hook
//! always answers `{"continue":true}`.
use super::hook_input::{self, whole, Field};
use crate::ai_sessions::git_snapshot::SnapshotKind;
use serde_json::Value;

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

const PROMPT_FIELDS: &[Field] = &[
    whole("conversation_id"),
    whole("generation_id"),
    whole("model"),
    whole("model_id"),
    whole("workspace_roots"),
    whole("prompt"),
];
const SESSION_FIELDS: &[Field] = &[
    whole("hook_event_name"),
    whole("conversation_id"),
    whole("generation_id"),
    whole("model"),
    whole("model_id"),
    whole("workspace_roots"),
    whole("text"),
    whole("status"),
];

/// The directory a Cursor hook ran for: its first workspace root. Cursor
/// sends no `cwd` with its agent hooks, so this is the only directory whose
/// recording policy can govern what the hook carries.
pub(super) fn workspace(event: &Value) -> Option<&str> {
    event["workspace_roots"]
        .as_array()?
        .iter()
        .find_map(|root| root.as_str().filter(|root| !root.is_empty()))
}

fn conversation(event: &Value) -> Option<&str> {
    event["conversation_id"]
        .as_str()
        .filter(|id| crate::util::is_valid_session_id(id))
}

/// Handle `beforeSubmitPrompt`: cache the prompt for the commands it leads
/// to, store it in the session, and let Cursor proceed whatever happened.
pub fn handle_hook_prompt() -> Result<()> {
    let result = capture_prompt();
    println!("{{\"continue\":true}}");
    result
}

fn capture_prompt() -> Result<()> {
    let Some(payload) = hook_input::read(std::io::stdin().lock(), PROMPT_FIELDS)? else {
        return Ok(());
    };
    payload.require(payload.event["prompt"].is_string())?;
    let mut event = payload.event;
    let Some(conversation) = conversation(&event).map(str::to_owned) else {
        return Ok(());
    };
    let session_id = format!("cursor-{conversation}");
    let workspace = workspace(&event).map(str::to_owned);
    crate::commands::agent_session::record_git_snapshot(
        &session_id,
        workspace.as_deref(),
        SnapshotKind::Start,
    );
    let prompt = event["prompt"].as_str().unwrap_or_default();
    if prompt.is_empty() {
        return Ok(());
    }
    if !crate::config::is_paused() {
        cache_prompt(&session_id, prompt, workspace.as_deref())?;
    }
    if let Some(workspace) = workspace {
        event["hook_event_name"] = "beforeSubmitPrompt".into();
        capture(&conversation, &event, &workspace);
    }
    Ok(())
}

/// Cache the prompt, redacted and cut with its workspace's policy, for
/// `afterShellExecution` to attach to the commands it leads to.
fn cache_prompt(session_id: &str, prompt: &str, workspace: Option<&str>) -> Result<()> {
    let dir = workspace.map_or_else(
        || std::env::current_dir().unwrap_or_default(),
        std::path::PathBuf::from,
    );
    let cfg = crate::config::capture_config_for_dir(&dir);
    if !cfg.enabled {
        return Ok(());
    }
    let safe = if cfg.redaction.enabled {
        crate::redact::redact_secrets_with_extra(prompt, &cfg.redaction.extra_patterns)
    } else {
        prompt.to_owned()
    };
    let truncated = crate::util::truncate_str(&safe, cfg.agent.prompt_capture_max_chars, "...");
    let prompts_dir = super::get_prompts_dir()?;
    std::fs::create_dir_all(&prompts_dir)?;
    let prompt_file = prompts_dir.join(format!("{session_id}.prompt"));
    crate::util::atomic_write_with_mode(&prompt_file, &truncated, 0o600)?;
    Ok(())
}

/// Handle `afterAgentResponse` and `stop`: store the response or how the
/// turn ended, and take the turn-end git snapshot.
pub fn handle_hook_session() -> Result<()> {
    let Some(payload) = hook_input::read(std::io::stdin().lock(), SESSION_FIELDS)? else {
        return Ok(());
    };
    let is_stop = payload.event["hook_event_name"] == "stop";
    payload.require(is_stop || payload.event["text"].is_string())?;
    let event = payload.event;
    let Some(conversation) = conversation(&event) else {
        return Ok(());
    };
    let workspace = workspace(&event);
    if is_stop {
        crate::commands::agent_session::record_git_snapshot(
            &format!("cursor-{conversation}"),
            workspace,
            SnapshotKind::End,
        );
    }
    if let Some(workspace) = workspace {
        capture(conversation, &event, workspace);
    }
    Ok(())
}

/// Store the event `payload` describes, if any. Never fails the hook.
fn capture(conversation: &str, payload: &Value, workspace: &str) {
    let at = chrono::Utc::now().timestamp_millis();
    let Some(event) = crate::ai_sessions::cursor::event_from_payload(payload, workspace, at) else {
        return;
    };
    if let Err(error) = crate::commands::agent_session::import_cursor_event(conversation, event) {
        eprintln!("suvadu: session capture: {error}");
    }
}

/// The directory a command reported by `afterShellExecution` ran in: its
/// own `cwd` when Cursor sends one, else the workspace root.
pub(super) fn command_cwd(event: &Value) -> &str {
    event["cwd"]
        .as_str()
        .filter(|cwd| !cwd.is_empty())
        .or_else(|| workspace(event))
        .unwrap_or(".")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn the_workspace_is_the_first_usable_root() {
        assert_eq!(
            workspace(&json!({"workspace_roots": ["", "/a", "/b"]})),
            Some("/a")
        );
        assert_eq!(workspace(&json!({"workspace_roots": []})), None);
        assert_eq!(workspace(&json!({"workspace_roots": "/a"})), None);
        assert_eq!(workspace(&json!({})), None);
    }

    #[test]
    fn a_command_runs_in_its_own_cwd_else_the_workspace() {
        assert_eq!(
            command_cwd(&json!({"cwd": "/w/sub", "workspace_roots": ["/w"]})),
            "/w/sub"
        );
        assert_eq!(command_cwd(&json!({"workspace_roots": ["/w"]})), "/w");
        assert_eq!(command_cwd(&json!({})), ".");
    }

    #[test]
    fn only_safe_conversation_ids_name_a_session() {
        assert_eq!(
            conversation(&json!({"conversation_id": "c-1"})),
            Some("c-1")
        );
        assert_eq!(conversation(&json!({"conversation_id": "../x"})), None);
        assert_eq!(conversation(&json!({"conversation_id": 7})), None);
    }
}
