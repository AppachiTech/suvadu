//! Codex hooks use Claude-compatible event names but have distinct identity and semantics.
use std::collections::HashMap;
use std::io::Read;
use std::path::{Path, PathBuf};

use crate::{config, util};
use serde_json::{json, Value};

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

pub fn handle_hook() -> Result<()> {
    let mut input = String::new();
    std::io::stdin()
        .take(super::MAX_HOOK_INPUT_BYTES + 1)
        .read_to_string(&mut input)?;
    if input.is_empty() {
        return Ok(());
    }
    if input.len() as u64 > super::MAX_HOOK_INPUT_BYTES {
        return Err("Codex hook input exceeds 1 MB".into());
    }
    let event: Value = serde_json::from_str(&input)?;
    let Some(session) = event["session_id"].as_str().filter(|id| valid_id(id)) else {
        return Ok(());
    };
    if config::is_paused() {
        return Ok(());
    }
    let cfg = config::load_config_cached()?;
    if !cfg.enabled {
        return Ok(());
    }
    let turn = event["turn_id"].as_str().filter(|id| valid_id(id));
    let prompts_dir = super::get_prompts_dir()?.join("codex").join(session);
    match event["hook_event_name"].as_str() {
        Some("UserPromptSubmit") => {
            let (Some(turn), Some(prompt)) = (turn, event["prompt"].as_str()) else {
                return Ok(());
            };
            let prompt = if cfg.redaction.enabled {
                crate::redact::redact_secrets_with_extra(prompt, &cfg.redaction.extra_patterns)
            } else {
                prompt.to_string()
            };
            let prompt = util::truncate_str(&prompt, cfg.agent.prompt_capture_max_chars, "...");
            std::fs::create_dir_all(&prompts_dir)?;
            util::atomic_write_with_mode(
                &prompts_dir.join(format!("{turn}.prompt")),
                &prompt,
                0o600,
            )?;
            Ok(())
        }
        Some("PostToolUse") if event["tool_name"] == "Bash" => {
            record_command(&event, session, turn, &prompts_dir)
        }
        _ => Ok(()),
    }
}

fn valid_id(id: &str) -> bool {
    // Also bounds each on-disk filename and the prefixed database session ID.
    id.len() <= 128 && util::is_valid_session_id(id)
}

fn record_command(
    event: &Value,
    session: &str,
    turn: Option<&str>,
    prompts_dir: &Path,
) -> Result<()> {
    let Some(command) = event["tool_input"]["command"]
        .as_str()
        .filter(|s| !s.is_empty())
    else {
        return Ok(());
    };
    let cwd = event["tool_input"]["cwd"]
        .as_str()
        .or_else(|| event["tool_input"]["workdir"].as_str())
        .or_else(|| event["cwd"].as_str())
        .unwrap_or(".");
    let mut context = HashMap::new();
    if let Some(turn) = turn {
        context.insert("codex_turn_id".into(), turn.into());
        if let Ok(prompt) = std::fs::read_to_string(prompts_dir.join(format!("{turn}.prompt"))) {
            if !prompt.is_empty() {
                context.insert("agent_prompt".into(), prompt);
            }
        }
    }
    if let Some(call) = event["tool_use_id"].as_str().filter(|id| valid_id(id)) {
        context.insert("codex_tool_use_id".into(), call.into());
    }
    // PostToolUse also fires for failures. Raw command output isn't reliable exit metadata.
    let exit_code = event["tool_response"]["exit_code"]
        .as_i64()
        .and_then(|code| i32::try_from(code).ok());
    let now = chrono::Utc::now().timestamp_millis();
    context.insert("timing_source".into(), "hook_received".into());
    crate::commands::entry::handle_add_with_context(crate::commands::entry::AddParams {
        session_id: format!("codex-{session}"),
        command: command.into(),
        cwd: cwd.into(),
        exit_code,
        started_at: now,
        ended_at: now,
        executor_type: Some("agent".into()),
        executor: Some("openai-codex".into()),
        context: Some(context),
    })
}

pub fn handle_init() -> Result<()> {
    let home = std::env::var_os("HOME").ok_or("HOME is not set")?;
    let codex_home = std::env::var_os("CODEX_HOME")
        .filter(|p| !p.is_empty())
        .map_or_else(|| PathBuf::from(&home).join(".codex"), PathBuf::from);
    let hooks_dir = PathBuf::from(home).join(".config/suvadu/hooks");
    let path = codex_home.join("hooks.json");
    let before = if path.exists() {
        Some(std::fs::read_to_string(&path)?)
    } else {
        None
    };
    let mut settings: Value = before
        .as_deref()
        .map(serde_json::from_str)
        .transpose()?
        .unwrap_or_else(|| json!({}));
    let script_path = hooks_dir.join("codex.sh");
    merge_hooks(&mut settings, &script_path, &hooks_dir)?;
    let binary = std::env::current_exe()?;
    let script = super::agent_hook::script(&binary.to_string_lossy(), "hook-codex", "codex");
    std::fs::create_dir_all(&hooks_dir)?;
    std::fs::create_dir_all(&codex_home)?;
    let updated = format!("{}\n", serde_json::to_string_pretty(&settings)?);
    if before.as_deref() != Some(&updated) {
        if let Some(before) = before {
            // Retain each replaced version, without overwriting an earlier backup.
            let backup =
                codex_home.join(format!("hooks.json.suvadu-backup-{}", uuid::Uuid::new_v4()));
            util::atomic_write_with_mode(&backup, &before, 0o600)?;
            println!("Previous hooks saved to {}", backup.display());
        }
    }
    util::atomic_write_with_mode(&script_path, &script, 0o700)?;
    util::atomic_write_with_mode(&path, &updated, 0o600)?;
    println!(
        "Codex prompt and shell-command hooks configured in {}",
        path.display()
    );
    println!(
        "Restart Codex, then review and trust the Suvadu hooks when prompted (or use /hooks)."
    );
    println!("Prompts are cached locally per turn; recorded commands link to their turn's prompt.");
    println!("View commands: suv history --executor openai-codex");
    println!("View prompts with commands: suv agent prompts --executor openai-codex");
    Ok(())
}

/// Remove only known Suvadu scripts, preserving other handlers within a mixed group.
fn merge_hooks(settings: &mut Value, script: &Path, hooks_dir: &Path) -> Result<()> {
    remove_managed_hooks(settings, hooks_dir)?;
    let hooks = settings["hooks"]
        .as_object_mut()
        .ok_or("Codex hooks must be an object")?;
    let command = super::shell_escape(&script.to_string_lossy());
    for event in ["PostToolUse", "UserPromptSubmit"] {
        let mut group = json!({"hooks":[{"type":"command", "command":command, "timeout":10}]});
        if event == "PostToolUse" {
            group["matcher"] = "Bash".into();
        }
        hooks
            .entry(event)
            .or_insert_with(|| json!([]))
            .as_array_mut()
            .ok_or("Codex hook event must be an array")?
            .push(group);
    }
    Ok(())
}

fn remove_managed_hooks(settings: &mut Value, hooks_dir: &Path) -> Result<()> {
    let root = settings
        .as_object_mut()
        .ok_or("Codex hooks.json must contain an object")?;
    let hooks = root
        .entry("hooks")
        .or_insert_with(|| json!({}))
        .as_object_mut()
        .ok_or("Codex hooks must be an object")?;
    let managed = [
        "codex.sh",
        "claude-code-post-tool.sh",
        "claude-code-post-tool-failure.sh",
        "claude-code-prompt.sh",
    ];
    for event in ["PostToolUse", "PostToolUseFailure", "UserPromptSubmit"] {
        let Some(groups) = hooks.get_mut(event) else {
            continue;
        };
        let groups = groups
            .as_array_mut()
            .ok_or("Codex hook event must be an array")?;
        for group in groups.iter_mut() {
            let handlers = group
                .get_mut("hooks")
                .and_then(Value::as_array_mut)
                .ok_or("Codex hook group must contain a hooks array")?;
            handlers.retain(|handler| {
                !handler["command"].as_str().is_some_and(|command| {
                    managed.iter().any(|name| {
                        let known = hooks_dir.join(name).to_string_lossy().to_string();
                        command == known || command == super::shell_escape(&known)
                    })
                })
            });
        }
        groups.retain(|group| group["hooks"].as_array().is_some_and(|h| !h.is_empty()));
    }
    Ok(())
}

/// Remove our registrations before uninstall deletes their scripts.
pub fn cleanup() -> Result<bool> {
    let home = std::env::var_os("HOME").ok_or("HOME is not set")?;
    let codex_home = std::env::var_os("CODEX_HOME")
        .filter(|p| !p.is_empty())
        .map_or_else(|| PathBuf::from(&home).join(".codex"), PathBuf::from);
    cleanup_file(
        &codex_home.join("hooks.json"),
        &PathBuf::from(home).join(".config/suvadu/hooks"),
    )
}

fn cleanup_file(path: &Path, hooks_dir: &Path) -> Result<bool> {
    if !path.exists() {
        return Ok(false);
    }
    let before = std::fs::read_to_string(path)?;
    let original: Value = serde_json::from_str(&before)?;
    if original.get("hooks").is_none() {
        return Ok(false);
    }
    let mut updated = original.clone();
    remove_managed_hooks(&mut updated, hooks_dir)?;
    if updated == original {
        return Ok(false);
    }
    let backup = path.with_file_name(format!("hooks.json.suvadu-backup-{}", uuid::Uuid::new_v4()));
    util::atomic_write_with_mode(&backup, &before, 0o600)?;
    util::atomic_write_with_mode(
        path,
        &format!("{}\n", serde_json::to_string_pretty(&updated)?),
        0o600,
    )?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uninstall_preserves_other_hooks_and_backs_up_original() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("hooks.json");
        let hooks_dir = temp.path().join("hook scripts");
        let original = json!({"description":"keep", "hooks":{
            "PostToolUse":[{"matcher":"Bash","hooks":[
                {"type":"command","command":super::super::shell_escape(&hooks_dir.join("codex.sh").to_string_lossy())},
                {"type":"command","command":"/team/audit.sh"}]}],
            "Stop":[{"hooks":[{"type":"command","command":"/team/stop.sh"}]}]
        }});
        std::fs::write(&path, original.to_string()).unwrap();
        assert!(cleanup_file(&path, &hooks_dir).unwrap());
        let updated: Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(updated["description"], "keep");
        assert_eq!(updated["hooks"]["Stop"], original["hooks"]["Stop"]);
        assert_eq!(
            updated["hooks"]["PostToolUse"][0]["hooks"],
            json!([{"type":"command","command":"/team/audit.sh"}])
        );
        assert!(!cleanup_file(&path, &hooks_dir).unwrap());
        let backups: Vec<_> = std::fs::read_dir(temp.path())
            .unwrap()
            .flatten()
            .filter(|entry| {
                entry
                    .file_name()
                    .to_string_lossy()
                    .starts_with("hooks.json.suvadu-backup-")
            })
            .collect();
        assert_eq!(backups.len(), 1);
        assert_eq!(
            std::fs::read_to_string(backups[0].path()).unwrap(),
            original.to_string()
        );
    }
}
