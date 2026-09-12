//! Codex hooks use Claude-compatible event names but have distinct identity and semantics.
use std::collections::HashMap;
use std::io::Read;
use std::path::{Path, PathBuf};

use crate::{config, util};
use serde_json::{json, Value};
use util::atomic_write;

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
    if terminal_hook(
        &event,
        |path, id| crate::commands::agent_session::import_native(path, Some(id)),
        &mut std::io::stdout(),
        &mut std::io::stderr(),
    )? {
        return Ok(());
    }
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

fn terminal_hook(
    event: &Value,
    mut import: impl FnMut(&Path, &str) -> Result<Value>,
    output: &mut impl std::io::Write,
    errors: &mut impl std::io::Write,
) -> Result<bool> {
    let name = event["hook_event_name"].as_str().unwrap_or("");
    if !matches!(name, "Stop" | "SessionEnd") {
        return Ok(false);
    }
    if let (Some(id), Some(path)) = (
        event["session_id"].as_str().filter(|id| valid_id(id)),
        event["transcript_path"].as_str().filter(|p| !p.is_empty()),
    ) {
        match import(Path::new(path), id) {
            Err(error) => writeln!(errors, "suvadu: session capture: {error}")?,
            Ok(result) if result["has_more"] == true => writeln!(errors, "suvadu: more transcript data remains; a later hook or suv agent import-session will continue")?,
            Ok(_) => (),
        }
    }
    if name == "Stop" {
        writeln!(output, "{{}}")?;
    }
    Ok(true)
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
    let mut backup_path = None;
    if before.as_deref() != Some(&updated) {
        if let Some(before) = before {
            // Retain each replaced version, without overwriting an earlier backup.
            let backup =
                codex_home.join(format!("hooks.json.suvadu-backup-{}", uuid::Uuid::new_v4()));
            util::atomic_write_with_mode(&backup, &before, 0o600)?;
            backup_path = Some(backup);
        }
    }
    util::atomic_write_with_mode(&script_path, &script, 0o700)?;
    util::atomic_write_with_mode(&path, &updated, 0o600)?;
    let mcp_configured = try_configure_codex_mcp(&codex_home, &binary.to_string_lossy());
    let mcp_path = matches!(mcp_configured, Ok(true)).then(|| codex_home.join("config.toml"));
    print!(
        "{}",
        init_message(
            &path,
            backup_path.as_deref(),
            mcp_path.as_deref(),
            crate::util::color_enabled(),
        )
    );
    Ok(())
}

fn init_message(
    hooks_path: &Path,
    backup_path: Option<&Path>,
    mcp_path: Option<&Path>,
    color: bool,
) -> String {
    let (bold, reset) = if color {
        ("\x1b[1m", "\x1b[0m")
    } else {
        ("", "")
    };
    let green = if color { "\x1b[32m" } else { "" };
    let cyan = if color { "\x1b[36m" } else { "" };
    let mut lines = vec![
        format!("{bold}Suvadu — Codex Integration{reset}"),
        String::new(),
        format!(
            "{green}✓{reset} Hooks auto-configured: {}",
            hooks_path.display()
        ),
        "  Captures prompts, shell commands, responses, models, and reported token usage.".into(),
    ];
    if let Some(backup) = backup_path {
        lines.push(format!("  Previous hooks backed up: {}", backup.display()));
    }
    if let Some(mcp) = mcp_path {
        lines.extend([
            String::new(),
            format!(
                "{green}✓{reset} MCP server auto-configured: {}",
                mcp.display()
            ),
            "  Codex can now query your Suvadu history via MCP.".into(),
        ]);
    }
    lines.extend([
        String::new(),
        "Activate in Codex:".into(),
        "  1. In the Codex terminal CLI, open /hooks.".into(),
        "  2. Review and trust the Suvadu hooks, including Stop and SessionEnd.".into(),
        "  3. Relaunch Codex.".into(),
        "     VS Code: fully quit and reopen VS Code after trusting the hooks.".into(),
        String::new(),
        "After one fresh Codex turn, try:".into(),
        format!("  {cyan}suv agent sessions{reset}                              — browse captured AI sessions"),
        format!("  {cyan}suv agent prompts --executor openai-codex{reset}     — see prompts and their commands"),
        format!("  {cyan}suv history --executor openai-codex{reset}           — see commands executed by Codex"),
        String::new(),
        "Your AI agent can also query Suvadu through MCP — try asking it:".into(),
        format!("  {cyan}\"Summarize my latest Codex session.\"{reset}"),
    ]);
    format!("{}\n", lines.join("\n"))
}

/// Sets `table[key]`, preserving the existing value's comment/formatting decor
/// (a trailing `# comment`, spacing, etc.) when the value already matches.
fn set_toml_value_keeping_decor(table: &mut toml_edit::Table, key: &str, new: toml_edit::Value) {
    let bare = |v: &toml_edit::Value| v.clone().decorated("", "").to_string();
    match table.get_mut(key).and_then(toml_edit::Item::as_value_mut) {
        Some(existing) if bare(existing) == bare(&new) => {}
        Some(existing) => {
            let decor = existing.decor().clone();
            *existing = new;
            *existing.decor_mut() = decor;
        }
        _ => table[key] = toml_edit::Item::Value(new),
    }
}

/// Auto-configure the MCP server in Codex's `config.toml`.
///
/// Edits in place with `toml_edit` so the rest of the file survives untouched —
/// comments, key order and formatting included. `config.toml` is a
/// hand-maintained file, so a parse-and-reserialize round trip would silently
/// strip every comment in it.
///
/// The `command`/`args` we own are refreshed on every run so a re-install picks
/// up a moved `suv` binary, but the file is only written when that actually
/// changes something.
fn try_configure_codex_mcp(codex_home: &Path, bin_path: &str) -> Result<bool> {
    let config_path = codex_home.join("config.toml");
    let existing = std::fs::read_to_string(&config_path).unwrap_or_default();
    let mut doc: toml_edit::DocumentMut = existing.parse()?;

    let fresh_servers = !doc.contains_key("mcp_servers");
    let servers = doc
        .entry("mcp_servers")
        .or_insert_with(|| toml_edit::Item::Table(toml_edit::Table::new()))
        .as_table_mut()
        .ok_or("mcp_servers is not a table")?;
    if fresh_servers {
        servers.set_implicit(true);
    }

    let server = servers
        .entry("suvadu")
        .or_insert_with(|| toml_edit::Item::Table(toml_edit::Table::new()))
        .as_table_mut()
        .ok_or("mcp_servers.suvadu is not a table")?;
    set_toml_value_keeping_decor(server, "command", bin_path.into());
    let mut args = toml_edit::Array::new();
    args.push("mcp-serve");
    set_toml_value_keeping_decor(server, "args", args.into());

    let updated = doc.to_string();
    if updated != existing {
        atomic_write(&config_path, &updated)?;
    }
    Ok(true)
}

/// Remove only known Suvadu scripts, preserving other handlers within a mixed group.
fn merge_hooks(settings: &mut Value, script: &Path, hooks_dir: &Path) -> Result<()> {
    remove_managed_hooks(settings, hooks_dir)?;
    let hooks = settings["hooks"]
        .as_object_mut()
        .ok_or("Codex hooks must be an object")?;
    let command = super::shell_escape(&script.to_string_lossy());
    for event in ["PostToolUse", "UserPromptSubmit", "Stop", "SessionEnd"] {
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
    for event in [
        "PostToolUse",
        "PostToolUseFailure",
        "UserPromptSubmit",
        "Stop",
        "SessionEnd",
    ] {
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
    fn init_message_groups_configuration_activation_and_next_steps() {
        let message = init_message(
            Path::new("/home/test/.codex/hooks.json"),
            Some(Path::new("/home/test/.codex/hooks.json.suvadu-backup-test")),
            Some(Path::new("/home/test/.codex/config.toml")),
            false,
        );

        assert!(message.starts_with("Suvadu — Codex Integration\n\n"));
        assert!(message.contains("✓ Hooks auto-configured: /home/test/.codex/hooks.json"));
        assert!(message.contains("Captures prompts, shell commands, responses, models"));
        assert!(message.contains("Previous hooks backed up:"));
        assert!(message.contains("✓ MCP server auto-configured: /home/test/.codex/config.toml"));
        assert!(message.contains("Activate in Codex:"));
        assert!(message.contains("1. In the Codex terminal CLI, open /hooks."));
        assert!(message.contains("fully quit and reopen VS Code"));
        assert!(message.contains("After one fresh Codex turn, try:"));
        assert!(message.contains("suv agent sessions"));
        assert!(message.contains("suv agent prompts --executor openai-codex"));
        assert!(message.contains("Summarize my latest Codex session."));
    }

    #[test]
    fn init_message_omits_optional_configuration_lines_when_absent() {
        let message = init_message(Path::new("/home/test/.codex/hooks.json"), None, None, false);

        assert!(!message.contains("Previous hooks backed up:"));
        assert!(!message.contains("MCP server auto-configured:"));
    }

    #[test]
    fn terminal_hooks_are_installed_idempotently_and_removed_without_other_handlers() {
        let hooks_dir = Path::new("/tmp/synthetic hooks");
        let script = hooks_dir.join("codex.sh");
        let mut settings = json!({"hooks":{
            "Stop":[{"hooks":[{"type":"command","command":"/team/stop.sh"}]}],
            "SessionEnd":[{"hooks":[{"type":"command","command":"/team/end.sh"}]}]
        }});
        merge_hooks(&mut settings, &script, hooks_dir).unwrap();
        let once = settings.clone();
        merge_hooks(&mut settings, &script, hooks_dir).unwrap();
        assert_eq!(settings, once);
        for event in ["Stop", "SessionEnd"] {
            assert_eq!(settings["hooks"][event].as_array().unwrap().len(), 2);
            assert_eq!(
                settings["hooks"][event][1]["hooks"][0]["command"],
                super::super::shell_escape(&script.to_string_lossy())
            );
        }
        remove_managed_hooks(&mut settings, hooks_dir).unwrap();
        assert_eq!(
            settings["hooks"]["Stop"][0]["hooks"][0]["command"],
            "/team/stop.sh"
        );
        assert_eq!(
            settings["hooks"]["SessionEnd"][0]["hooks"][0]["command"],
            "/team/end.sh"
        );
        assert_eq!(settings["hooks"]["Stop"].as_array().unwrap().len(), 1);
        assert_eq!(settings["hooks"]["SessionEnd"].as_array().unwrap().len(), 1);
    }

    #[test]
    fn stop_imports_once_and_returns_empty_protocol_object_on_failure() {
        let event = json!({"hook_event_name":"Stop","session_id":"test-session","transcript_path":"/tmp/synthetic-session.jsonl"});
        let mut output = Vec::new();
        let mut errors = Vec::new();
        let mut imports = Vec::new();
        assert!(terminal_hook(
            &event,
            |path, id| {
                imports.push((path.to_path_buf(), id.to_string()));
                Err("synthetic import failure".into())
            },
            &mut output,
            &mut errors
        )
        .unwrap());
        assert_eq!(
            imports,
            vec![(
                PathBuf::from("/tmp/synthetic-session.jsonl"),
                "test-session".to_string()
            )]
        );
        assert_eq!(output, b"{}\n");
        assert!(String::from_utf8(errors)
            .unwrap()
            .contains("synthetic import failure"));
    }

    #[test]
    fn session_end_import_is_bounded_even_when_more_records_remain() {
        let event = json!({"hook_event_name":"SessionEnd","session_id":"test-session","transcript_path":"/tmp/synthetic-session.jsonl"});
        let mut calls = 0;
        let mut output = Vec::new();
        assert!(terminal_hook(
            &event,
            |_, _| {
                calls += 1;
                Ok(json!({"has_more":true}))
            },
            &mut output,
            &mut Vec::new()
        )
        .unwrap());
        assert_eq!(calls, 1);
        assert!(output.is_empty());
    }

    #[test]
    fn stop_without_transcript_or_with_invalid_identity_does_not_import() {
        for event in [
            json!({"hook_event_name":"Stop","session_id":"test-session","transcript_path":null}),
            json!({"hook_event_name":"Stop","session_id":"../bad","transcript_path":"/tmp/x"}),
        ] {
            let mut output = Vec::new();
            assert!(terminal_hook(
                &event,
                |_, _| panic!("must not import"),
                &mut output,
                &mut Vec::new()
            )
            .unwrap());
            assert_eq!(output, b"{}\n");
        }
    }

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

    #[test]
    fn configure_mcp_is_idempotent_and_preserves_comments() {
        let temp = tempfile::tempdir().unwrap();
        let codex_home = temp.path();

        // First run creates config.toml with our server.
        let first = try_configure_codex_mcp(codex_home, "/usr/local/bin/suv").unwrap();
        assert!(first);
        let config_path = codex_home.join("config.toml");
        let content = std::fs::read_to_string(&config_path).unwrap();

        let parsed: toml::Value = toml::from_str(&content).unwrap();
        let server = &parsed["mcp_servers"]["suvadu"];
        assert_eq!(server["command"].as_str(), Some("/usr/local/bin/suv"));
        assert_eq!(
            server["args"].as_array().unwrap(),
            &vec![toml::Value::String("mcp-serve".into())]
        );

        // A stale binary path and an unrelated key added by hand.
        let mut stale: toml::Value = toml::from_str(&content).unwrap();
        let server = stale["mcp_servers"]["suvadu"].as_table_mut().unwrap();
        server.insert(
            "command".to_string(),
            toml::Value::String("/stale/bin/suv".to_string()),
        );
        server.insert("startup_timeout_sec".to_string(), toml::Value::Integer(15));
        std::fs::write(&config_path, toml::to_string_pretty(&stale).unwrap()).unwrap();

        // Re-running refreshes our launcher without touching other settings.
        try_configure_codex_mcp(codex_home, "/opt/homebrew/bin/suv").unwrap();
        let content2 = std::fs::read_to_string(&config_path).unwrap();
        let parsed2: toml::Value = toml::from_str(&content2).unwrap();
        let servers = parsed2["mcp_servers"].as_table().unwrap();
        assert_eq!(servers.len(), 1, "should not duplicate the suvadu server");
        assert_eq!(
            servers["suvadu"]["command"].as_str(),
            Some("/opt/homebrew/bin/suv")
        );
        assert_eq!(
            servers["suvadu"]["startup_timeout_sec"].as_integer(),
            Some(15)
        );

        // A hand-maintained file's comments and other tables must survive untouched.
        let hand_written = concat!(
            "# my codex config\n",
            "# keep these comments\n",
            "model = \"gpt-5.6-sol\"\n",
            "\n",
            "[mcp_servers.context7]\n",
            "command = \"npx\"\n",
            "args = [\"-y\", \"@upstash/context7-mcp\"]\n",
            "\n",
            "# suvadu: local shell history\n",
            "[mcp_servers.suvadu]\n",
            "command = \"/old/bin/suv\" # stale path\n",
            "args = [\"mcp-serve\"] # leave me alone\n",
        );
        std::fs::write(&config_path, hand_written).unwrap();
        try_configure_codex_mcp(codex_home, "/new/bin/suv").unwrap();
        let refreshed = std::fs::read_to_string(&config_path).unwrap();
        assert_eq!(
            refreshed,
            hand_written.replace("/old/bin/suv", "/new/bin/suv")
        );

        // A no-op rerun must not touch the file at all.
        let mtime_before = std::fs::metadata(&config_path).unwrap().modified().unwrap();
        try_configure_codex_mcp(codex_home, "/new/bin/suv").unwrap();
        assert_eq!(std::fs::read_to_string(&config_path).unwrap(), refreshed);
        assert_eq!(
            std::fs::metadata(&config_path).unwrap().modified().unwrap(),
            mtime_before
        );
    }
}
