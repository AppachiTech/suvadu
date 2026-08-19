//! IDE and agent integration hooks for Suvadu.
//!
//! Contains Claude Code hook handlers, IDE init routines (Cursor, Antigravity),
//! and helpers for merging settings into `~/.claude/settings.json`.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use crate::util::atomic_write;

/// Maximum bytes to read from stdin for hook input (1 MB).
const MAX_HOOK_INPUT_BYTES: u64 = 1_048_576;

/// Read a hook event from stdin, capped at `MAX_HOOK_INPUT_BYTES`.
fn read_hook_event() -> Result<serde_json::Value, Box<dyn std::error::Error>> {
    use std::io::Read;

    let mut input = String::new();
    std::io::stdin()
        .take(MAX_HOOK_INPUT_BYTES)
        .read_to_string(&mut input)?;
    Ok(serde_json::from_str(&input)?)
}

/// Record a Bash tool call from an agent hook event on stdin.
///
/// Claude Code (`PostToolUse`, `PostToolUseFailure`) and Codex (`PostToolUse`)
/// all deliver the same payload keys — `tool_name`, `tool_input.command`, `cwd`,
/// `session_id` — verified against codex-cli 0.148. They differ only in how the
/// exit code is reported (Codex does not report one at all), hence the
/// `exit_code` closure. `prefix` namespaces the session id against zsh sessions,
/// and `executor` is the agent recorded on the entry.
///
/// An absent or unsafe `session_id` still records the command under a synthetic
/// one — losing the command from history is worse than losing prompt grouping.
/// The prompt hooks deliberately do the opposite and drop the event: a prompt
/// cached under an id no command will ever look up is dead weight.
fn record_agent_bash_hook(
    prefix: &str,
    executor: &str,
    exit_code: impl FnOnce(&serde_json::Value) -> Option<i32>,
) -> Result<(), Box<dyn std::error::Error>> {
    let event = read_hook_event()?;

    // Only process Bash tool calls
    let tool_name = event
        .get("tool_name")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("");
    if tool_name != "Bash" {
        return Ok(());
    }

    let command = event
        .get("tool_input")
        .and_then(|ti| ti.get("command"))
        .and_then(serde_json::Value::as_str)
        .unwrap_or("");
    if command.is_empty() {
        return Ok(());
    }

    let cwd = event
        .get("cwd")
        .and_then(serde_json::Value::as_str)
        .unwrap_or(".");

    let session_id = event
        .get("session_id")
        .and_then(serde_json::Value::as_str)
        .filter(|s| is_valid_session_id(s))
        .map_or_else(
            || format!("{prefix}-{}", uuid::Uuid::new_v4()),
            |s| format!("{prefix}-{s}"),
        );

    let now = chrono::Utc::now().timestamp_millis();

    let context = get_cached_prompt(&session_id).map(|prompt| {
        let mut ctx = HashMap::new();
        ctx.insert("agent_prompt".to_string(), prompt);
        ctx
    });

    crate::commands::entry::handle_add_with_context(crate::commands::entry::AddParams {
        session_id,
        command: command.to_string(),
        cwd: cwd.to_string(),
        exit_code: exit_code(&event),
        started_at: now,
        ended_at: now,
        executor_type: Some("agent".to_string()),
        executor: Some(executor.to_string()),
        context,
    })
}

/// Cache the prompt text from a `UserPromptSubmit` hook event on stdin.
///
/// Drops events whose `session_id` is absent or unsafe: the id becomes a file
/// name, and no command lookup would ever match it anyway.
fn cache_agent_prompt(prefix: &str) -> Result<(), Box<dyn std::error::Error>> {
    let event = read_hook_event()?;

    let session_id = event
        .get("session_id")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("");
    if !is_valid_session_id(session_id) {
        return Ok(());
    }

    let prompt = event
        .get("prompt")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("");
    if prompt.is_empty() {
        return Ok(());
    }

    // Store prompt in cache file (atomic write to avoid corruption on crash)
    let prompts_dir = get_prompts_dir()?;
    std::fs::create_dir_all(&prompts_dir)?;
    let prompt_file = prompts_dir.join(format!("{prefix}-{session_id}.prompt"));
    // Cap length to keep the cache lightweight (configurable).
    let max_chars =
        crate::config::load_config_cached().map_or(4000, |c| c.agent.prompt_capture_max_chars);
    let truncated = crate::util::truncate_str(prompt, max_chars, "...");
    atomic_write(&prompt_file, &truncated)?;

    // Restrict prompt cache file to owner-only (contains user prompts)
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&prompt_file, std::fs::Permissions::from_mode(0o600));
    }

    Ok(())
}

/// Read an explicit exit code out of a `PostToolUse` payload's `tool_response`.
fn tool_response_exit_code(event: &serde_json::Value) -> Option<i32> {
    event
        .get("tool_response")
        .and_then(|tr| {
            tr.get("exit_code")
                .or_else(|| tr.get("exitCode"))
                .or_else(|| tr.get("status_code"))
        })
        .and_then(serde_json::Value::as_i64)
        .and_then(|ec| i32::try_from(ec).ok())
}

/// Derive an exit code from a Codex `PostToolUse` event.
///
/// codex-cli 0.148 exposes no exit code at all: it fires `PostToolUse` for both
/// success and failure with an identical key set, and `tool_response` is a plain
/// string (stdout on success, stderr text on failure). So in practice this
/// returns `None` — unknown — rather than guessing 0 and labelling failed
/// commands as successful.
///
/// The structured reads below are forward-compat for a Codex that starts
/// reporting one, and are not exercised by any payload Codex sends today: an
/// explicit `exit_code`/`exitCode`/`status_code` on a `tool_response` object
/// wins, else a non-null `error` (on `tool_response` or top-level) counts as a
/// failure whose code is parsed out of the message, defaulting to 1.
fn codex_exit_code(event: &serde_json::Value) -> Option<i32> {
    tool_response_exit_code(event).or_else(|| {
        event
            .get("error")
            .or_else(|| event.get("tool_response").and_then(|tr| tr.get("error")))
            .filter(|err| !err.is_null())
            .map(|err| {
                err.as_str()
                    .and_then(parse_exit_code_from_error)
                    .unwrap_or(1)
            })
    })
}

/// Handle `PostToolUse` hook from Claude Code — records the command.
///
/// Only fires on success, so we default to exit 0; the `tool_response` read is
/// forward-compat for when Claude Code starts reporting it.
pub fn handle_hook_claude_code() -> Result<(), Box<dyn std::error::Error>> {
    record_agent_bash_hook("claude", "claude-code", |event| {
        tool_response_exit_code(event).or(Some(0))
    })
}

/// Handle `UserPromptSubmit` hook from Claude Code — caches the prompt text.
pub fn handle_hook_claude_prompt() -> Result<(), Box<dyn std::error::Error>> {
    cache_agent_prompt("claude")
}

/// Handle `PostToolUseFailure` hook from Claude Code — records failed commands.
///
/// Fires when a Bash command exits non-zero. The payload carries an `error`
/// string (no `tool_response`) from which we parse the exit code.
pub fn handle_hook_claude_code_failure() -> Result<(), Box<dyn std::error::Error>> {
    record_agent_bash_hook("claude", "claude-code", |event| {
        event
            .get("error")
            .and_then(serde_json::Value::as_str)
            .and_then(parse_exit_code_from_error)
            .or(Some(1))
    })
}

/// Handle Codex `PostToolUse` hook event — records the command.
///
/// Codex runs `PostToolUse` after both successful and failed Bash calls, so a
/// single handler captures both.
pub fn handle_hook_codex() -> Result<(), Box<dyn std::error::Error>> {
    record_agent_bash_hook("codex", "openai-codex", codex_exit_code)
}

/// Handle Codex `UserPromptSubmit` hook event — caches the prompt text.
///
/// codex-cli 0.148 sends `session_id`, `turn_id`, `transcript_path`, `cwd`,
/// `model`, `permission_mode` and `prompt` — the same `session_id` the matching
/// `PostToolUse` events carry, which is what makes prompt grouping work.
pub fn handle_hook_codex_prompt() -> Result<(), Box<dyn std::error::Error>> {
    cache_agent_prompt("codex")
}

/// Handle `afterShellExecution` hook from Cursor — reads JSON event from stdin and records the command.
///
/// Cursor's payload: `{ "command": "...", "output": "...", "exit_code": 0, "cwd": "...",
///   "duration": 123, "conversation_id": "...", "generation_id": "..." }`
pub fn handle_hook_cursor() -> Result<(), Box<dyn std::error::Error>> {
    use std::io::Read;

    let mut input = String::new();
    std::io::stdin()
        .take(MAX_HOOK_INPUT_BYTES)
        .read_to_string(&mut input)?;

    let event: serde_json::Value = serde_json::from_str(&input)?;

    let command = event
        .get("command")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("");
    if command.is_empty() {
        return Ok(());
    }

    let cwd = event
        .get("cwd")
        .and_then(serde_json::Value::as_str)
        .unwrap_or(".");

    let exit_code = event
        .get("exit_code")
        .and_then(serde_json::Value::as_i64)
        .and_then(|ec| i32::try_from(ec).ok())
        .or(Some(0));

    let duration_ms = event
        .get("duration")
        .and_then(serde_json::Value::as_i64)
        .unwrap_or(0);

    // Use conversation_id as session, prefixed to avoid collision
    let session_id = event
        .get("conversation_id")
        .and_then(serde_json::Value::as_str)
        .filter(|s| is_valid_session_id(s))
        .map_or_else(
            || format!("cursor-{}", uuid::Uuid::new_v4()),
            |s| format!("cursor-{s}"),
        );

    let now = chrono::Utc::now().timestamp_millis();
    let started_at = now - duration_ms;

    // Read cached prompt for this session (set by beforeSubmitPrompt hook)
    let context = get_cached_prompt(&session_id).map(|prompt| {
        let mut ctx = HashMap::new();
        ctx.insert("agent_prompt".to_string(), prompt);
        ctx
    });

    crate::commands::entry::handle_add_with_context(crate::commands::entry::AddParams {
        session_id,
        command: command.to_string(),
        cwd: cwd.to_string(),
        exit_code,
        started_at,
        ended_at: now,
        executor_type: Some("agent".to_string()),
        executor: Some("cursor".to_string()),
        context,
    })
}

/// Handle `beforeSubmitPrompt` hook from Cursor — caches the user prompt.
///
/// Payload: `{ "prompt": "...", "conversation_id": "...", ... }`
/// Must return `{"continue": true}` on stdout to let Cursor proceed.
pub fn handle_hook_cursor_prompt() -> Result<(), Box<dyn std::error::Error>> {
    use std::io::Read;

    let mut input = String::new();
    std::io::stdin()
        .take(MAX_HOOK_INPUT_BYTES)
        .read_to_string(&mut input)?;

    let event: serde_json::Value = serde_json::from_str(&input)?;

    let conversation_id = event
        .get("conversation_id")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("");
    if !is_valid_session_id(conversation_id) {
        println!("{{\"continue\":true}}");
        return Ok(());
    }

    let prompt = event
        .get("prompt")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("");

    if !prompt.is_empty() {
        let session_id = format!("cursor-{conversation_id}");
        let prompts_dir = get_prompts_dir()?;
        std::fs::create_dir_all(&prompts_dir)?;
        let prompt_file = prompts_dir.join(format!("{session_id}.prompt"));
        let max_chars =
            crate::config::load_config_cached().map_or(4000, |c| c.agent.prompt_capture_max_chars);
        let truncated = crate::util::truncate_str(prompt, max_chars, "...");
        atomic_write(&prompt_file, &truncated)?;

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(&prompt_file, std::fs::Permissions::from_mode(0o600));
        }
    }

    // Must respond to let Cursor proceed
    println!("{{\"continue\":true}}");
    Ok(())
}

/// Parse an exit code from Claude Code's error string.
/// Examples: "Command exited with non-zero status code 1", "status code 127"
fn parse_exit_code_from_error(error: &str) -> Option<i32> {
    // Look for "status code N" or "exit code N" patterns
    let patterns = ["status code ", "exit code "];
    for pat in patterns {
        if let Some(pos) = error.to_lowercase().find(pat) {
            let after = &error[pos + pat.len()..];
            let num_str: String = after.chars().take_while(char::is_ascii_digit).collect();
            if let Ok(code) = num_str.parse::<i32>() {
                return Some(code);
            }
        }
    }
    None
}

/// Returns `true` if `id` contains only safe characters for use in file names.
fn is_valid_session_id(id: &str) -> bool {
    crate::util::is_valid_session_id(id)
}

/// Get the directory for cached agent prompts (uses cached project dirs)
fn get_prompts_dir() -> Result<PathBuf, Box<dyn std::error::Error>> {
    let dirs = crate::util::project_dirs().ok_or("Could not determine data directory")?;
    Ok(dirs.data_dir().join("prompts"))
}

/// Read the cached prompt for a session (if any)
fn get_cached_prompt(session_id: &str) -> Option<String> {
    let prompts_dir = get_prompts_dir().ok()?;
    let prompt_file = prompts_dir.join(format!("{session_id}.prompt"));
    std::fs::read_to_string(prompt_file).ok()
}

/// Auto-configure the MCP server in Claude Code's `~/.claude.json`.
/// Adds `suvadu` to the top-level `mcpServers` if not already present.
fn try_configure_claude_mcp(bin_path: &str) -> Result<bool, Box<dyn std::error::Error>> {
    let home = std::env::var("HOME")?;
    let claude_json = PathBuf::from(&home).join(".claude.json");

    let mut config: serde_json::Value = if claude_json.exists() {
        let content = std::fs::read_to_string(&claude_json)?;
        serde_json::from_str(&content)?
    } else {
        serde_json::json!({})
    };

    let obj = config
        .as_object_mut()
        .ok_or(".claude.json root is not an object")?;
    let servers = obj
        .entry("mcpServers")
        .or_insert_with(|| serde_json::json!({}));
    let servers_obj = servers
        .as_object_mut()
        .ok_or("mcpServers is not an object")?;

    if servers_obj.contains_key("suvadu") {
        return Ok(true); // already configured
    }

    servers_obj.insert(
        "suvadu".to_string(),
        serde_json::json!({
            "type": "stdio",
            "command": bin_path,
            "args": ["mcp-serve"],
            "env": {}
        }),
    );

    let updated = serde_json::to_string_pretty(&config)?;
    atomic_write(&claude_json, &updated)?;
    Ok(true)
}

/// Auto-configure the MCP server in Cursor's `~/.cursor/mcp.json`.
/// Adds `suvadu` to `mcpServers` if not already present.
fn try_configure_cursor_mcp(bin_path: &str) -> Result<bool, Box<dyn std::error::Error>> {
    let home = std::env::var("HOME")?;
    let cursor_dir = PathBuf::from(&home).join(".cursor");
    std::fs::create_dir_all(&cursor_dir)?;
    let mcp_json = cursor_dir.join("mcp.json");

    let mut config: serde_json::Value = if mcp_json.exists() {
        let content = std::fs::read_to_string(&mcp_json)?;
        serde_json::from_str(&content)?
    } else {
        serde_json::json!({})
    };

    let obj = config
        .as_object_mut()
        .ok_or("mcp.json root is not an object")?;
    let servers = obj
        .entry("mcpServers")
        .or_insert_with(|| serde_json::json!({}));
    let servers_obj = servers
        .as_object_mut()
        .ok_or("mcpServers is not an object")?;

    if servers_obj.contains_key("suvadu") {
        return Ok(true);
    }

    servers_obj.insert(
        "suvadu".to_string(),
        serde_json::json!({
            "command": bin_path,
            "args": ["mcp-serve"]
        }),
    );

    let updated = serde_json::to_string_pretty(&config)?;
    atomic_write(&mcp_json, &updated)?;
    Ok(true)
}

/// Print post-install tips showing users what to try next.
/// `has_prompts` indicates whether this integration captures prompts.
fn print_post_install_tips(cyan: &str, r: &str, has_prompts: bool, has_mcp: bool) {
    println!();
    println!("After your next agent session, try:");
    if has_prompts {
        println!(
            "  {cyan}suv agent prompts{r}     \u{2014} see what prompts triggered which commands"
        );
    }
    println!("  {cyan}suv agent dashboard{r}   \u{2014} real-time agent activity monitor");
    if has_mcp {
        println!();
        println!("Your AI agent can also query history directly \u{2014} try asking it:");
        println!("  {cyan}\"What commands failed in this project recently?\"{r}");
    }
}

/// Check if a hook command path belongs to suvadu. Uses path-separator-aware
/// matching to avoid false positives on paths like `/usr/bin/not-suvadu-tool`.
fn is_suvadu_hook_command(cmd: &str) -> bool {
    // Match /suvadu/ as a path component, or the binary names "suv"/"suvadu" at end of path
    cmd.contains("/suvadu/")
        || cmd.ends_with("/suv")
        || cmd.ends_with("/suvadu")
        || cmd.starts_with("suv ")
        || cmd.starts_with("suvadu ")
}

/// Shell-escape a string for embedding inside single quotes.
/// Replaces `'` with `'\''` (end quote, literal quote, restart quote).
fn shell_escape(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

fn codex_hook_script(
    bin_path: &str,
    hook_name: &str,
    description: &str,
    subcommand: &str,
) -> String {
    let escaped_bin = shell_escape(bin_path);
    format!(
        "#!/bin/bash\n\
         # Suvadu — Codex {hook_name} Hook\n\
         # {description}\n\
         # Generated by: suv init codex\n\
         _SUVADU_BIN={escaped_bin}\n\
         if [[ ! -x \"$_SUVADU_BIN\" ]]; then\n\
           _SUVADU_BIN=$(type -P suv 2>/dev/null) || exit 0\n\
         fi\n\
         exec \"$_SUVADU_BIN\" {subcommand} 2>/dev/null\n"
    )
}

/// Install Claude Code hooks and configure `~/.claude/settings.json`.
pub fn handle_init_claude_code() -> Result<(), Box<dyn std::error::Error>> {
    let current_exe = std::env::current_exe()?;
    let bin_path = current_exe.to_string_lossy().to_string();

    // Create hooks directory
    let home = std::env::var("HOME")?;
    let hooks_dir = PathBuf::from(&home)
        .join(".config")
        .join("suvadu")
        .join("hooks");
    std::fs::create_dir_all(&hooks_dir)?;

    // Write the PostToolUse hook script (single-quoted path for shell safety)
    let hook_script_path = hooks_dir.join("claude-code-post-tool.sh");
    let escaped_bin = shell_escape(&bin_path);
    let script = format!(
        "#!/bin/bash\n\
         # Suvadu — Claude Code PostToolUse Hook\n\
         # Records AI-executed commands in your shell history\n\
         # Generated by: suv init claude-code\n\
         exec {escaped_bin} hook-claude-code 2>/dev/null\n"
    );
    crate::util::atomic_write_with_mode(&hook_script_path, &script, 0o700)?;

    // Write the PostToolUseFailure hook script
    let failure_hook_path = hooks_dir.join("claude-code-post-tool-failure.sh");
    let failure_script = format!(
        "#!/bin/bash\n\
         # Suvadu — Claude Code PostToolUseFailure Hook\n\
         # Records failed AI-executed commands in your shell history\n\
         # Generated by: suv init claude-code\n\
         exec {escaped_bin} hook-claude-code-failure 2>/dev/null\n"
    );
    crate::util::atomic_write_with_mode(&failure_hook_path, &failure_script, 0o700)?;

    // Write the UserPromptSubmit hook script
    let prompt_hook_path = hooks_dir.join("claude-code-prompt.sh");
    let prompt_script = format!(
        "#!/bin/bash\n\
         # Suvadu — Claude Code UserPromptSubmit Hook\n\
         # Captures the user prompt for agent command grouping\n\
         # Generated by: suv init claude-code\n\
         exec {escaped_bin} hook-claude-prompt 2>/dev/null\n"
    );
    crate::util::atomic_write_with_mode(&prompt_hook_path, &prompt_script, 0o700)?;

    let hook_path_str = hook_script_path.to_string_lossy().to_string();
    let failure_hook_path_str = failure_hook_path.to_string_lossy().to_string();
    let prompt_hook_path_str = prompt_hook_path.to_string_lossy().to_string();

    // Try auto-merge into ~/.claude/settings.json
    let settings_path = PathBuf::from(&home).join(".claude").join("settings.json");

    let auto_configured = try_merge_claude_settings(
        &settings_path,
        &hook_path_str,
        &failure_hook_path_str,
        &prompt_hook_path_str,
    );

    let color = crate::util::color_enabled();
    let (b, r) = if color {
        ("\x1b[1m", "\x1b[0m")
    } else {
        ("", "")
    };
    let green = if color { "\x1b[32m" } else { "" };
    let cyan = if color { "\x1b[36m" } else { "" };
    println!("{b}Suvadu — Claude Code Integration{r}");
    println!();
    println!("Hook scripts installed:");
    println!("  {hook_path_str}");
    println!("  {failure_hook_path_str}");
    println!("  {prompt_hook_path_str}");
    println!();

    if matches!(auto_configured, Ok(true)) {
        println!(
            "{green}✓{r} Settings auto-configured: {}",
            settings_path.display()
        );
        println!();
        println!("Restart Claude Code to activate.");
    } else {
        println!("Add this to ~/.claude/settings.json:");
        println!();
        println!(
            "{}",
            generate_claude_settings_snippet(
                &hook_path_str,
                &failure_hook_path_str,
                &prompt_hook_path_str,
            )
        );
        println!();
        println!("Then restart Claude Code to activate.");
    }

    // Auto-configure MCP server
    let mcp_configured = try_configure_claude_mcp(&bin_path);
    if matches!(mcp_configured, Ok(true)) {
        println!("{green}\u{2713}{r} MCP server auto-configured in ~/.claude.json");
        println!("  AI agents can now query your shell history via MCP.");
    }

    println!();
    println!("Verify with: {cyan}suv search --executor agent{r}");
    print_post_install_tips(cyan, r, true, true);

    Ok(())
}

/// Generate the JSON snippet for Claude Code settings.
pub fn generate_claude_settings_snippet(
    hook_path: &str,
    failure_hook_path: &str,
    prompt_hook_path: &str,
) -> String {
    serde_json::to_string_pretty(&serde_json::json!({
        "hooks": {
            "PostToolUse": [{
                "matcher": "Bash",
                "hooks": [{
                    "type": "command",
                    "command": hook_path
                }]
            }],
            "PostToolUseFailure": [{
                "matcher": "Bash",
                "hooks": [{
                    "type": "command",
                    "command": failure_hook_path
                }]
            }],
            "UserPromptSubmit": [{
                "hooks": [{
                    "type": "command",
                    "command": prompt_hook_path
                }]
            }]
        }
    }))
    .unwrap_or_default()
}

/// Remove duplicate suvadu hook entries from both `PostToolUse` and `UserPromptSubmit`.
/// Returns true if any duplicates were removed.
fn dedup_suvadu_hooks(settings: &mut serde_json::Value) -> bool {
    let mut changed = false;
    for key in ["PostToolUse", "PostToolUseFailure", "UserPromptSubmit"] {
        let Some(arr) = settings
            .get_mut("hooks")
            .and_then(|h| h.get_mut(key))
            .and_then(serde_json::Value::as_array_mut)
        else {
            continue;
        };
        let mut seen_suvadu = false;
        let before = arr.len();
        arr.retain(|group| {
            let is_suvadu = group
                .get("hooks")
                .and_then(serde_json::Value::as_array)
                .is_some_and(|hooks| {
                    hooks.iter().any(|h| {
                        h.get("command")
                            .and_then(serde_json::Value::as_str)
                            .is_some_and(is_suvadu_hook_command)
                    })
                });
            if is_suvadu {
                if seen_suvadu {
                    return false; // drop duplicate
                }
                seen_suvadu = true;
            }
            true
        });
        if arr.len() != before {
            changed = true;
        }
    }
    changed
}

/// Check if a suvadu hook already exists for a given hook type (e.g. `PostToolUse`, `UserPromptSubmit`).
fn has_suvadu_hook(settings: &serde_json::Value, hook_type: &str) -> bool {
    settings
        .get("hooks")
        .and_then(|h| h.get(hook_type))
        .and_then(serde_json::Value::as_array)
        .is_some_and(|arr| {
            arr.iter().any(|group| {
                group
                    .get("hooks")
                    .and_then(serde_json::Value::as_array)
                    .is_some_and(|hooks| {
                        hooks.iter().any(|h| {
                            h.get("command")
                                .and_then(serde_json::Value::as_str)
                                .is_some_and(is_suvadu_hook_command)
                        })
                    })
            })
        })
}

/// Same as `has_suvadu_hook` but operates on the hooks map directly.
fn has_suvadu_hook_in_obj(
    hooks_obj: &serde_json::Map<String, serde_json::Value>,
    hook_type: &str,
) -> bool {
    hooks_obj
        .get(hook_type)
        .and_then(serde_json::Value::as_array)
        .is_some_and(|arr| {
            arr.iter().any(|group| {
                group
                    .get("hooks")
                    .and_then(serde_json::Value::as_array)
                    .is_some_and(|hooks| {
                        hooks.iter().any(|h| {
                            h.get("command")
                                .and_then(serde_json::Value::as_str)
                                .is_some_and(is_suvadu_hook_command)
                        })
                    })
            })
        })
}

/// Add a hook entry to the settings object under the given hook type.
fn add_hook_entry(
    hooks_obj: &mut serde_json::Map<String, serde_json::Value>,
    hook_type: &str,
    hook_json: serde_json::Value,
) -> Result<(), Box<dyn std::error::Error>> {
    let arr = hooks_obj
        .entry(hook_type)
        .or_insert_with(|| serde_json::json!([]));
    arr.as_array_mut()
        .ok_or_else(|| format!("{hook_type} is not an array"))?
        .push(hook_json);
    Ok(())
}

/// Try to merge Suvadu hooks into an existing Claude settings file.
pub fn try_merge_claude_settings(
    settings_path: &Path,
    hook_path: &str,
    failure_hook_path: &str,
    prompt_hook_path: &str,
) -> Result<bool, Box<dyn std::error::Error>> {
    if !settings_path.exists() {
        return Ok(false);
    }

    let content = std::fs::read_to_string(settings_path)?;
    let mut settings: serde_json::Value = serde_json::from_str(&content)?;

    let already_has_post_tool = has_suvadu_hook(&settings, "PostToolUse");

    if already_has_post_tool {
        // Add any missing hooks
        let obj = settings
            .as_object_mut()
            .ok_or("settings.json root is not an object")?;
        let hooks = obj.entry("hooks").or_insert_with(|| serde_json::json!({}));
        let hooks_obj = hooks.as_object_mut().ok_or("hooks is not an object")?;
        let added_failure = if has_suvadu_hook_in_obj(hooks_obj, "PostToolUseFailure") {
            false
        } else {
            add_hook_entry(
                hooks_obj,
                "PostToolUseFailure",
                serde_json::json!({
                    "matcher": "Bash",
                    "hooks": [{
                        "type": "command",
                        "command": failure_hook_path
                    }]
                }),
            )?;
            true
        };

        let added_prompt = if has_suvadu_hook_in_obj(hooks_obj, "UserPromptSubmit") {
            false
        } else {
            add_hook_entry(
                hooks_obj,
                "UserPromptSubmit",
                serde_json::json!({
                    "hooks": [{
                        "type": "command",
                        "command": prompt_hook_path
                    }]
                }),
            )?;
            true
        };

        // Also deduplicate
        let deduped = dedup_suvadu_hooks(&mut settings);
        if added_failure || added_prompt || deduped {
            let updated = serde_json::to_string_pretty(&settings)?;
            atomic_write(settings_path, &updated)?;
        }
        return Ok(true);
    }

    // Add all hooks
    let obj = settings
        .as_object_mut()
        .ok_or("settings.json root is not an object")?;
    let hooks = obj.entry("hooks").or_insert_with(|| serde_json::json!({}));
    let hooks_obj = hooks.as_object_mut().ok_or("hooks is not an object")?;

    add_hook_entry(
        hooks_obj,
        "PostToolUse",
        serde_json::json!({
            "matcher": "Bash",
            "hooks": [{
                "type": "command",
                "command": hook_path
            }]
        }),
    )?;

    add_hook_entry(
        hooks_obj,
        "PostToolUseFailure",
        serde_json::json!({
            "matcher": "Bash",
            "hooks": [{
                "type": "command",
                "command": failure_hook_path
            }]
        }),
    )?;

    add_hook_entry(
        hooks_obj,
        "UserPromptSubmit",
        serde_json::json!({
            "hooks": [{
                "type": "command",
                "command": prompt_hook_path
            }]
        }),
    )?;

    // Write back with pretty formatting
    let updated = serde_json::to_string_pretty(&settings)?;
    atomic_write(settings_path, &updated)?;

    Ok(true)
}

/// `OpenCode` plugin script content.
/// This JS plugin uses opencode's `tool.execute.after` hook to record every
/// bash command in suvadu with the correct executor metadata. It also captures
/// user prompts via the `event` hook for agent command grouping.
const OPENCODE_PLUGIN_SCRIPT: &str = r#"// Suvadu — OpenCode Integration Plugin
// Records AI-executed commands in your shell history
// Generated by: suv init opencode
import { spawnSync } from "child_process";
import { writeFileSync, mkdirSync, readFileSync } from "fs";
import { join } from "path";

// Track user message IDs so we can match prompts to the right messages
const userMessageIDs = new Set();
const sessionPrompts = new Map();

// Prompts cache directory (same location suvadu uses for claude-code prompts)
function getPromptsDir() {
  const platform = process.platform;
  const home = process.env.HOME || process.env.USERPROFILE || "";
  if (platform === "darwin") {
    return join(home, "Library", "Application Support", "tech.appachi.suvadu", "prompts");
  }
  return join(home, ".local", "share", "suvadu", "prompts");
}

function cachePrompt(sessionID, prompt) {
  try {
    const dir = getPromptsDir();
    mkdirSync(dir, { recursive: true });
    const truncated = prompt.length > 500 ? prompt.slice(0, 500) + "..." : prompt;
    writeFileSync(join(dir, `opencode-${sessionID}.prompt`), truncated, { mode: 0o600 });
  } catch {}
}

export default async (input) => ({
  event: async (eventInput) => {
    const evt = eventInput?.event;
    if (!evt) return;

    // Track user messages so we can identify their prompts
    if (evt.type === "message.updated") {
      const info = evt.properties?.info;
      if (info?.role === "user" && info?.id) {
        userMessageIDs.add(info.id);
      }
    }

    // Capture user prompt text
    if (evt.type === "message.part.updated") {
      const part = evt.properties?.part;
      if (part?.type === "text" && part?.text && part?.messageID && userMessageIDs.has(part.messageID)) {
        const sessionID = part.sessionID || "unknown";
        sessionPrompts.set(sessionID, part.text);
        cachePrompt(sessionID, part.text);
        userMessageIDs.delete(part.messageID);
      }
    }
  },
  "tool.execute.after": async (toolInput, output) => {
    if (toolInput.tool !== "bash") return;
    const cmd = toolInput.args?.command;
    if (!cmd) return;
    const now = String(Date.now());
    const cwd = toolInput.args?.workdir || process.cwd();
    const sessionID = "opencode-" + (toolInput.sessionID || "unknown");
    const args = [
      "add",
      "--session-id", sessionID,
      "--command", cmd,
      "--cwd", cwd,
      "--started-at", now,
      "--ended-at", now,
      "--executor-type", "agent",
      "--executor", "opencode",
    ];
    const exit = output?.metadata?.exit;
    if (exit !== undefined && exit !== null) {
      args.push("--exit-code", String(exit));
    }
    try {
      spawnSync("suv", args, { stdio: "ignore", timeout: 5000 });
    } catch {
      // Best-effort — don't break opencode if suvadu fails
    }
  },
});
"#;

/// pi.dev extension script content.
/// This TypeScript extension uses pi.dev's event system to record bash commands
/// and capture user prompts. It subscribes to `before_agent_start` for prompt capture
/// and `tool_result` for command recording.
const PI_EXTENSION_SCRIPT: &str = r#"// Suvadu — pi.dev Integration Extension
// Records AI-executed commands in your shell history
// Generated by: suv init pi
import { spawnSync } from "child_process";
import { writeFileSync, mkdirSync, existsSync, readFileSync } from "fs";
import { join, basename } from "path";
import type { ExtensionAPI } from "@mariozechner/pi-coding-agent";

// Track pending bash calls: toolCallId -> { command, cwd, startTime, sessionId }
const pendingBash = new Map<
  string,
  { command: string; cwd: string; startTime: number; sessionId: string; prompt?: string }
>();

// Prompts cache directory
function getPromptsDir(): string {
  const platform = process.platform;
  const home = process.env.HOME || process.env.USERPROFILE || "";
  if (platform === "darwin") {
    return join(home, "Library", "Application Support", "tech.appachi.suvadu", "prompts");
  }
  return join(home, ".local", "share", "suvadu", "prompts");
}

function cachePrompt(sessionId: string, prompt: string): void {
  try {
    const dir = getPromptsDir();
    mkdirSync(dir, { recursive: true });
    const truncated = prompt.length > 500 ? prompt.slice(0, 500) + "..." : prompt;
    writeFileSync(join(dir, `pi-${sessionId}.prompt`), truncated, { mode: 0o600 });
  } catch {
    // Best-effort — ignore failures
  }
}

function getCachedPrompt(sessionId: string): string | undefined {
  try {
    const dir = getPromptsDir();
    const path = join(dir, `pi-${sessionId}.prompt`);
    if (existsSync(path)) {
      return readFileSync(path, "utf8");
    }
  } catch {}
  return undefined;
}

// Get session ID from context or derive from file
function getSessionId(ctx: { sessionManager: { getSessionId?: () => string; getSessionFile?: () => string | undefined } }): string {
  // Prefer the session UUID from the session manager
  if (ctx.sessionManager.getSessionId) {
    const id = ctx.sessionManager.getSessionId();
    if (id) return id;
  }
  // Fallback: extract from filename (timestamp_uuid.jsonl)
  const sessionFile = ctx.sessionManager.getSessionFile?.();
  if (sessionFile) {
    const base = basename(sessionFile, ".jsonl");
    // Filename pattern: <timestamp>_<uuid>.jsonl - extract the UUID part
    const parts = base.split("_");
    if (parts.length >= 2) {
      return parts[parts.length - 1]; // The UUID part
    }
    return base;
  }
  return "unknown";
}

// Record a command via suvadu CLI
function recordCommand(params: {
  sessionId: string;
  command: string;
  cwd: string;
  startTime: number;
  endTime: number;
  exitCode: number | null;
  prompt?: string;
}): void {
  const args = [
    "add",
    "--session-id",
    `pi-${params.sessionId}`,
    "--command",
    params.command,
    "--cwd",
    params.cwd,
    "--started-at",
    String(params.startTime),
    "--ended-at",
    String(params.endTime),
    "--executor-type",
    "agent",
    "--executor",
    "pi",
  ];

  if (params.exitCode !== null) {
    args.push("--exit-code", String(params.exitCode));
  }

  try {
    spawnSync("suv", args, { stdio: "ignore", timeout: 5000 });
  } catch {
    // Best-effort — don't break pi if suvadu fails
  }
}

export default function (pi: ExtensionAPI): void {
  // Capture user prompts before each agent turn
  pi.on("before_agent_start", async (event, ctx) => {
    const sessionId = getSessionId(ctx);
    const prompt = event.prompt || "";

    if (prompt && sessionId !== "unknown") {
      cachePrompt(sessionId, prompt);
    }
  });

  // Track bash tool calls before execution
  pi.on("tool_call", async (event, ctx) => {
    if (event.toolName !== "bash") return;

    const sessionId = getSessionId(ctx);
    const prompt = getCachedPrompt(sessionId);

    pendingBash.set(event.toolCallId, {
      command: (event.input as { command?: string }).command || "",
      cwd: ctx.cwd,
      startTime: Date.now(),
      sessionId,
      prompt,
    });
  });

  // Record bash commands after execution
  pi.on("tool_result", async (event, _ctx) => {
    if (event.toolName !== "bash") return;

    const pending = pendingBash.get(event.toolCallId);
    if (!pending) return;
    pendingBash.delete(event.toolCallId);

    // Extract exit code from result
    // pi.dev's bash tool doesn't expose exit code directly in details
    // For successful commands: isError is false, exit code is 0
    // For failed commands: isError is true, we try to find exit code in details
    let exitCode: number | null = null;
    if (event.isError) {
      // Try to find exit code in various possible fields, default to 1 if not found
      if (event.details && typeof event.details === "object") {
        const details = event.details as Record<string, unknown>;
        exitCode = (details.exitCode ?? details.exit_code ?? details.code ?? details.status ?? 1) as number;
      } else {
        exitCode = 1;
      }
    } else {
      // Successful command
      exitCode = 0;
    }

    recordCommand({
      sessionId: pending.sessionId,
      command: pending.command,
      cwd: pending.cwd,
      startTime: pending.startTime,
      endTime: Date.now(),
      exitCode,
      prompt: pending.prompt,
    });
  });
}
"#;
///
/// `OpenCode` automatically loads plugins from `~/.opencode/plugins/*.{ts,js}`.
/// This writes a small JS plugin that uses the `tool.execute.after` hook to
/// call `suv add` after every bash command opencode executes.
pub fn handle_init_opencode() -> Result<(), Box<dyn std::error::Error>> {
    let home = std::env::var("HOME")?;

    // Write plugin to opencode's global plugins directory
    let plugins_dir = PathBuf::from(&home).join(".opencode").join("plugins");
    std::fs::create_dir_all(&plugins_dir)?;

    let plugin_path = plugins_dir.join("suvadu.js");
    crate::util::atomic_write(&plugin_path, OPENCODE_PLUGIN_SCRIPT)?;

    let color = crate::util::color_enabled();
    let (b, r) = if color {
        ("\x1b[1m", "\x1b[0m")
    } else {
        ("", "")
    };
    let green = if color { "\x1b[32m" } else { "" };
    let cyan = if color { "\x1b[36m" } else { "" };

    println!("{b}Suvadu \u{2014} OpenCode Integration{r}");
    println!();
    println!(
        "{green}\u{2713}{r} Plugin installed: {}",
        plugin_path.display()
    );
    println!();
    println!("OpenCode will automatically load this plugin on next start.");
    println!("Commands executed by OpenCode will be recorded with executor=opencode.");
    println!();
    println!("Verify with: {cyan}suv search --executor opencode{r}");
    print_post_install_tips(cyan, r, true, false);

    Ok(())
}

/// Install the suvadu extension for pi.dev.
///
/// pi.dev automatically loads extensions from `~/.pi/agent/extensions/*.ts`.
/// This writes a TypeScript extension that subscribes to `before_agent_start` for
/// prompt capture and `tool_result` for bash command recording.
pub fn handle_init_pi() -> Result<(), Box<dyn std::error::Error>> {
    let home = std::env::var("HOME")?;

    // Write extension to pi.dev's global extensions directory
    let extensions_dir = PathBuf::from(&home)
        .join(".pi")
        .join("agent")
        .join("extensions");
    std::fs::create_dir_all(&extensions_dir)?;

    let extension_path = extensions_dir.join("suvadu.ts");
    crate::util::atomic_write(&extension_path, PI_EXTENSION_SCRIPT)?;

    let color = crate::util::color_enabled();
    let (b, r) = if color {
        ("\x1b[1m", "\x1b[0m")
    } else {
        ("", "")
    };
    let green = if color { "\x1b[32m" } else { "" };
    let cyan = if color { "\x1b[36m" } else { "" };

    println!("{b}Suvadu \u{2014} pi.dev Integration{r}");
    println!();
    println!(
        "{green}\u{2713}{r} Extension installed: {}",
        extension_path.display()
    );
    println!();
    println!("pi.dev will automatically load this extension on next start.");
    println!("Commands executed by pi.dev will be recorded with executor=pi.");
    println!();
    println!("Verify with: {cyan}suv search --executor pi{r}");
    print_post_install_tips(cyan, r, true, false);

    Ok(())
}

/// Add a suvadu hook group to a Codex hooks event, unless such a group already exists.
fn add_codex_hook_group(
    hooks_obj: &mut serde_json::Map<String, serde_json::Value>,
    event: &str,
    group: serde_json::Value,
) -> Result<bool, Box<dyn std::error::Error>> {
    if has_suvadu_hook_in_obj(hooks_obj, event) {
        return Ok(false);
    }
    add_hook_entry(hooks_obj, event, group)?;
    Ok(true)
}

/// Merge suvadu hooks into an existing Codex hooks.json, preserving any other
/// hooks the user already configured (Claude Code and Cursor behave this way).
/// Idempotent — re-running `suv init codex` does not duplicate suvadu hooks.
fn try_merge_codex_hooks(
    hooks_json_path: &Path,
    hook_path: &str,
    prompt_hook_path: &str,
) -> Result<bool, Box<dyn std::error::Error>> {
    let mut config: serde_json::Value = if hooks_json_path.exists() {
        let content = std::fs::read_to_string(hooks_json_path)?;
        serde_json::from_str(&content)?
    } else {
        serde_json::json!({})
    };

    let hooks_obj = config
        .as_object_mut()
        .ok_or("hooks.json root is not an object")?
        .entry("hooks")
        .or_insert_with(|| serde_json::json!({}));
    let hooks_obj = hooks_obj.as_object_mut().ok_or("hooks is not an object")?;

    let post_tool_group = serde_json::json!({
        "matcher": "^Bash$",
        "hooks": [{"type": "command", "command": hook_path}]
    });
    let prompt_group = serde_json::json!({
        "hooks": [{"type": "command", "command": prompt_hook_path}]
    });

    let added_post = add_codex_hook_group(hooks_obj, "PostToolUse", post_tool_group)?;
    let added_prompt = add_codex_hook_group(hooks_obj, "UserPromptSubmit", prompt_group)?;

    if added_post || added_prompt {
        let updated = serde_json::to_string_pretty(&config)?;
        atomic_write(hooks_json_path, &updated)?;
    }
    Ok(true)
}

/// Generate the JSON snippet for Codex hooks.json (matches the shape
/// `try_merge_codex_hooks` writes).
fn generate_codex_hooks_snippet(hook_path: &str, prompt_hook_path: &str) -> String {
    serde_json::to_string_pretty(&serde_json::json!({
        "hooks": {
            "PostToolUse": [{
                "matcher": "^Bash$",
                "hooks": [{"type": "command", "command": hook_path}]
            }],
            "UserPromptSubmit": [{
                "hooks": [{"type": "command", "command": prompt_hook_path}]
            }]
        }
    }))
    .unwrap_or_default()
}

/// Set `key` in `table` to `new`, leaving the existing entry's surrounding decor
/// (whitespace and trailing comments) intact, and touching nothing at all when
/// the value already matches.
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

/// Auto-configure the MCP server in Codex's `~/.codex/config.toml`.
///
/// Edits in place with `toml_edit` so the rest of the file survives untouched —
/// comments, key order and formatting included. Codex's `config.toml` is a
/// hand-maintained file, so a parse-and-reserialize round trip would silently
/// strip every comment in it.
///
/// The `command`/`args` we own are refreshed on every run so a re-install picks
/// up a moved `suv` binary, but the file is only written when that actually
/// changes something.
fn try_configure_codex_mcp(bin_path: &str) -> Result<bool, Box<dyn std::error::Error>> {
    let home = std::env::var("HOME")?;
    let codex_dir = PathBuf::from(&home).join(".codex");
    std::fs::create_dir_all(&codex_dir)?;
    let config_path = codex_dir.join("config.toml");

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

/// Install Codex hooks and configure `~/.codex/hooks.json` + MCP server.
pub fn handle_init_codex() -> Result<(), Box<dyn std::error::Error>> {
    let current_exe = std::env::current_exe()?;
    let bin_path = current_exe.to_string_lossy().to_string();

    let home = std::env::var("HOME")?;
    let hooks_dir = PathBuf::from(&home)
        .join(".config")
        .join("suvadu")
        .join("hooks");
    std::fs::create_dir_all(&hooks_dir)?;

    // Write the PostToolUse hook script
    let hook_script_path = hooks_dir.join("codex-post-tool.sh");
    let script = codex_hook_script(
        &bin_path,
        "PostToolUse",
        "Records AI-executed commands in your shell history",
        "hook-codex",
    );
    crate::util::atomic_write_with_mode(&hook_script_path, &script, 0o700)?;

    // Write the UserPromptSubmit hook script
    let prompt_hook_path = hooks_dir.join("codex-prompt.sh");
    let prompt_script = codex_hook_script(
        &bin_path,
        "UserPromptSubmit",
        "Captures the user prompt for agent command grouping",
        "hook-codex-prompt",
    );
    crate::util::atomic_write_with_mode(&prompt_hook_path, &prompt_script, 0o700)?;

    let hook_path_str = hook_script_path.to_string_lossy().to_string();
    let prompt_hook_path_str = prompt_hook_path.to_string_lossy().to_string();

    // Write ~/.codex/hooks.json to register the hooks with Codex, preserving
    // any hooks (e.g. synapse, plannotator) the user already configured.
    let codex_dir = PathBuf::from(&home).join(".codex");
    std::fs::create_dir_all(&codex_dir)?;
    let hooks_json_path = codex_dir.join("hooks.json");

    let auto_configured =
        try_merge_codex_hooks(&hooks_json_path, &hook_path_str, &prompt_hook_path_str);

    let color = crate::util::color_enabled();
    let (b, r) = if color {
        ("\x1b[1m", "\x1b[0m")
    } else {
        ("", "")
    };
    let green = if color { "\x1b[32m" } else { "" };
    let cyan = if color { "\x1b[36m" } else { "" };

    println!("{b}Suvadu \u{2014} Codex Integration{r}");
    println!();
    println!("Hook scripts installed:");
    println!("  {hook_path_str}");
    println!("  {prompt_hook_path_str}");
    println!();

    if matches!(auto_configured, Ok(true)) {
        println!(
            "{green}\u{2713}{r} Hooks merged into: {}",
            hooks_json_path.display()
        );
        println!();
    } else {
        println!(
            "Could not auto-merge into {} (existing file may be malformed).",
            hooks_json_path.display()
        );
        println!("Add this to ~/.codex/hooks.json:");
        println!();
        println!(
            "{}",
            generate_codex_hooks_snippet(&hook_path_str, &prompt_hook_path_str)
        );
        println!();
        println!("Then restart Codex to activate.");
    }

    // Auto-configure MCP server
    let mcp_configured = try_configure_codex_mcp(&bin_path);
    if matches!(mcp_configured, Ok(true)) {
        println!("{green}\u{2713}{r} MCP server auto-configured in ~/.codex/config.toml");
        println!("  AI agents can now query your shell history via MCP.");
    }

    println!();
    println!("{green}\u{2713}{r} Codex hooks require trust before they run. Start codex and run {cyan}/hooks{r}");
    println!("  to review and trust the suvadu hooks (PostToolUse + UserPromptSubmit).");
    println!();
    println!("Restart Codex to activate.");
    println!();
    println!("Verify with: {cyan}suv search --executor openai-codex{r}");
    print_post_install_tips(cyan, r, true, true);

    Ok(())
}

/// Set up Cursor AI agent integration via `afterShellExecution` hook.
pub fn handle_init_cursor() -> Result<(), Box<dyn std::error::Error>> {
    let current_exe = std::env::current_exe()?;
    let bin_path = current_exe.to_string_lossy().to_string();

    // Create hooks directory
    let home = std::env::var("HOME")?;
    let hooks_dir = PathBuf::from(&home)
        .join(".config")
        .join("suvadu")
        .join("hooks");
    std::fs::create_dir_all(&hooks_dir)?;

    let escaped_bin = shell_escape(&bin_path);

    // Write the afterShellExecution hook script
    let hook_script_path = hooks_dir.join("cursor-after-shell.sh");
    let script = format!(
        "#!/bin/bash\n\
         # Suvadu — Cursor afterShellExecution Hook\n\
         # Records AI-executed commands in your shell history\n\
         # Generated by: suv init cursor\n\
         exec {escaped_bin} hook-cursor 2>/dev/null\n"
    );
    crate::util::atomic_write_with_mode(&hook_script_path, &script, 0o700)?;

    // Write the beforeSubmitPrompt hook script
    let prompt_hook_path = hooks_dir.join("cursor-prompt.sh");
    let prompt_script = format!(
        "#!/bin/bash\n\
         # Suvadu — Cursor beforeSubmitPrompt Hook\n\
         # Captures the user prompt for agent command grouping\n\
         # Generated by: suv init cursor\n\
         exec {escaped_bin} hook-cursor-prompt 2>/dev/null\n"
    );
    crate::util::atomic_write_with_mode(&prompt_hook_path, &prompt_script, 0o700)?;

    let hook_path_str = hook_script_path.to_string_lossy().to_string();
    let prompt_hook_path_str = prompt_hook_path.to_string_lossy().to_string();

    // Try auto-merge into ~/.cursor/hooks.json
    let cursor_dir = PathBuf::from(&home).join(".cursor");
    std::fs::create_dir_all(&cursor_dir)?;
    let hooks_json_path = cursor_dir.join("hooks.json");

    let auto_configured =
        try_merge_cursor_hooks(&hooks_json_path, &hook_path_str, &prompt_hook_path_str);

    let color = crate::util::color_enabled();
    let (b, r) = if color {
        ("\x1b[1m", "\x1b[0m")
    } else {
        ("", "")
    };
    let green = if color { "\x1b[32m" } else { "" };
    let cyan = if color { "\x1b[36m" } else { "" };

    println!("{b}Suvadu \u{2014} Cursor Integration{r}");
    println!();
    println!("Hook scripts installed:");
    println!("  {hook_path_str}");
    println!("  {prompt_hook_path_str}");
    println!();

    if matches!(auto_configured, Ok(true)) {
        println!(
            "{green}\u{2713}{r} Settings auto-configured: {}",
            hooks_json_path.display()
        );
        println!();
        println!("Restart Cursor to activate.");
    } else {
        println!("Add this to ~/.cursor/hooks.json:");
        println!();
        println!(
            "{}",
            generate_cursor_hooks_snippet(&hook_path_str, &prompt_hook_path_str)
        );
        println!();
        println!("Then restart Cursor to activate.");
    }

    // Auto-configure MCP server
    let mcp_configured = try_configure_cursor_mcp(&bin_path);
    if matches!(mcp_configured, Ok(true)) {
        println!("{green}\u{2713}{r} MCP server auto-configured in ~/.cursor/mcp.json");
        println!("  AI agents can now query your shell history via MCP.");
    }

    println!();
    println!("Verify with: {cyan}suv search --executor cursor{r}");
    print_post_install_tips(cyan, r, true, true);

    Ok(())
}

/// Generate the JSON snippet for Cursor hooks.json.
fn generate_cursor_hooks_snippet(hook_path: &str, prompt_hook_path: &str) -> String {
    serde_json::to_string_pretty(&serde_json::json!({
        "version": 1,
        "hooks": {
            "afterShellExecution": [{
                "command": hook_path
            }],
            "beforeSubmitPrompt": [{
                "command": prompt_hook_path
            }]
        }
    }))
    .unwrap_or_default()
}

/// Try to merge Suvadu hooks into an existing Cursor hooks.json.
fn try_merge_cursor_hooks(
    hooks_path: &Path,
    hook_path: &str,
    prompt_hook_path: &str,
) -> Result<bool, Box<dyn std::error::Error>> {
    let mut settings: serde_json::Value = if hooks_path.exists() {
        let content = std::fs::read_to_string(hooks_path)?;
        serde_json::from_str(&content)?
    } else {
        serde_json::json!({})
    };

    // Ensure top-level structure
    let obj = settings
        .as_object_mut()
        .ok_or("hooks.json root is not an object")?;
    obj.entry("version").or_insert(serde_json::json!(1));
    let hooks = obj.entry("hooks").or_insert_with(|| serde_json::json!({}));
    let hooks_obj = hooks.as_object_mut().ok_or("hooks is not an object")?;

    // Add afterShellExecution hook if missing
    add_cursor_hook_if_missing(hooks_obj, "afterShellExecution", hook_path)?;
    // Add beforeSubmitPrompt hook if missing
    add_cursor_hook_if_missing(hooks_obj, "beforeSubmitPrompt", prompt_hook_path)?;

    let updated = serde_json::to_string_pretty(&settings)?;
    atomic_write(hooks_path, &updated)?;
    Ok(true)
}

/// Add a suvadu hook entry to a Cursor hooks object if not already present.
fn add_cursor_hook_if_missing(
    hooks_obj: &mut serde_json::Map<String, serde_json::Value>,
    hook_type: &str,
    hook_path: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    let arr = hooks_obj
        .entry(hook_type)
        .or_insert_with(|| serde_json::json!([]))
        .as_array_mut()
        .ok_or_else(|| format!("{hook_type} is not an array"))?;

    let already_has = arr.iter().any(|entry| {
        entry
            .get("command")
            .and_then(serde_json::Value::as_str)
            .is_some_and(is_suvadu_hook_command)
    });

    if !already_has {
        arr.push(serde_json::json!({ "command": hook_path }));
    }
    Ok(())
}

/// Unified init handler for terminal-based IDE integrations (Antigravity, etc.).
///
/// These IDEs are detected via environment variables set in their integrated terminals.
/// Suvadu's shell hooks automatically pick them up — this command just verifies the setup.
///
/// # Arguments
/// * `name` — Display name of the IDE (e.g. "Cursor", "Antigravity")
/// * `detection_info` — Human-readable description of how Suvadu detects the IDE
/// * `verify_executor` — The executor name for `suv search --executor <name>`
pub fn handle_init_ide(
    name: &str,
    detection_info: &str,
    verify_executor: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    let color = crate::util::color_enabled();
    let (b, r) = if color {
        ("\x1b[1m", "\x1b[0m")
    } else {
        ("", "")
    };
    let green = if color { "\x1b[32m" } else { "" };
    let yellow = if color { "\x1b[33m" } else { "" };
    let cyan = if color { "\x1b[36m" } else { "" };

    println!("{b}Suvadu \u{2014} {name} Integration{r}");
    println!();

    // Check if shell hooks are configured
    let home = std::env::var("HOME")?;
    let zshrc_path = PathBuf::from(&home).join(".zshrc");
    let bashrc_path = PathBuf::from(&home).join(".bashrc");

    let zsh_ok = zshrc_path
        .exists()
        .then(|| std::fs::read_to_string(&zshrc_path).ok())
        .flatten()
        .is_some_and(|c| c.contains("suv init zsh"));

    let bash_ok = bashrc_path
        .exists()
        .then(|| std::fs::read_to_string(&bashrc_path).ok())
        .flatten()
        .is_some_and(|c| c.contains("suv init bash"));

    if zsh_ok || bash_ok {
        println!("{green}\u{2713}{r} Shell hooks detected:");
        if zsh_ok {
            println!("    Zsh:  ~/.zshrc");
        }
        if bash_ok {
            println!("    Bash: ~/.bashrc");
        }
        println!();
        println!("{green}\u{2713}{r} {name} commands are automatically tracked when you");
        println!("  run commands in {name}'s integrated terminal.");
        println!();
        for line in detection_info.lines() {
            println!("  {line}");
        }
    } else {
        println!("{yellow}!{r} Shell hooks not found. Set them up first:");
        println!();
        println!("  For Zsh:");
        println!("    echo 'eval \"$(suv init zsh)\"' >> ~/.zshrc && source ~/.zshrc");
        println!();
        println!("  For Bash:");
        println!("    echo 'eval \"$(suv init bash)\"' >> ~/.bashrc && source ~/.bashrc");
        println!();
        println!("  Then reopen {name} \u{2014} commands will be tracked automatically.");
    }

    println!();
    println!("Verify with: {cyan}suv search --executor {verify_executor}{r}");
    print_post_install_tips(cyan, r, false, false);

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_hook_claude_code_parses_bash_event() {
        let input = r#"{
            "session_id": "abc123",
            "cwd": "/home/user/project",
            "hook_event_name": "PostToolUse",
            "tool_name": "Bash",
            "tool_input": {"command": "cargo test", "description": "Run tests"},
            "tool_response": {"exit_code": 0}
        }"#;

        let event: serde_json::Value = serde_json::from_str(input).unwrap();
        assert_eq!(event["tool_name"], "Bash");
        assert_eq!(event["tool_input"]["command"], "cargo test");
        assert_eq!(event["cwd"], "/home/user/project");
        assert_eq!(event["session_id"], "abc123");
    }

    #[test]
    fn test_hook_claude_code_skips_non_bash() {
        let input = r#"{
            "session_id": "abc123",
            "cwd": "/home/user",
            "tool_name": "Write",
            "tool_input": {"file_path": "/tmp/test.txt", "content": "hello"}
        }"#;

        let event: serde_json::Value = serde_json::from_str(input).unwrap();
        assert_ne!(event["tool_name"].as_str().unwrap(), "Bash");
    }

    #[test]
    fn test_hook_claude_code_handles_missing_exit_code() {
        let input = r#"{
            "session_id": "abc123",
            "cwd": "/home/user",
            "tool_name": "Bash",
            "tool_input": {"command": "echo hello"},
            "tool_response": {"some_other_field": "value"}
        }"#;

        let event: serde_json::Value = serde_json::from_str(input).unwrap();
        let exit_code = event
            .get("tool_response")
            .and_then(|tr| tr.get("exit_code"))
            .and_then(serde_json::Value::as_i64);
        assert!(exit_code.is_none());
    }

    #[test]
    fn test_parse_exit_code_from_error_standard() {
        assert_eq!(
            parse_exit_code_from_error("Command exited with non-zero status code 1"),
            Some(1)
        );
        assert_eq!(
            parse_exit_code_from_error("non-zero status code 127"),
            Some(127)
        );
        assert_eq!(
            parse_exit_code_from_error("Process terminated with exit code 2"),
            Some(2)
        );
    }

    #[test]
    fn test_parse_exit_code_from_error_no_match() {
        assert_eq!(parse_exit_code_from_error("Something went wrong"), None);
        assert_eq!(parse_exit_code_from_error(""), None);
    }

    #[test]
    fn test_post_tool_use_defaults_to_exit_0() {
        // PostToolUse only fires on success, so missing exit_code should default to 0
        let input = r#"{
            "session_id": "abc123",
            "cwd": "/home/user",
            "tool_name": "Bash",
            "tool_input": {"command": "echo hello"},
            "tool_response": {"stdout": "hello\n", "stderr": "", "interrupted": false}
        }"#;

        let event: serde_json::Value = serde_json::from_str(input).unwrap();
        let exit_code = event
            .get("tool_response")
            .and_then(|tr| {
                tr.get("exit_code")
                    .or_else(|| tr.get("exitCode"))
                    .or_else(|| tr.get("status_code"))
            })
            .and_then(serde_json::Value::as_i64)
            .and_then(|ec| i32::try_from(ec).ok())
            .or(Some(0)); // the fix
        assert_eq!(exit_code, Some(0));
    }

    #[test]
    fn test_valid_session_ids() {
        assert!(is_valid_session_id("abc123"));
        assert!(is_valid_session_id("session-with-dashes"));
        assert!(is_valid_session_id("session_with_underscores"));
        assert!(is_valid_session_id("MiXeD-CaSe_123"));
    }

    #[test]
    fn test_invalid_session_ids() {
        assert!(!is_valid_session_id(""));
        assert!(!is_valid_session_id("../../etc/passwd"));
        assert!(!is_valid_session_id("session id with spaces"));
        assert!(!is_valid_session_id("session/slash"));
        assert!(!is_valid_session_id("session\0null"));
        assert!(!is_valid_session_id(&"a".repeat(257)));
    }

    #[test]
    fn test_generate_claude_settings_snippet() {
        let json = generate_claude_settings_snippet(
            "/usr/local/bin/hook.sh",
            "/usr/local/bin/failure-hook.sh",
            "/usr/local/bin/prompt-hook.sh",
        );
        let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert!(parsed["hooks"]["PostToolUse"].is_array());
        assert_eq!(parsed["hooks"]["PostToolUse"][0]["matcher"], "Bash");
        let cmd = parsed["hooks"]["PostToolUse"][0]["hooks"][0]["command"]
            .as_str()
            .unwrap();
        assert_eq!(cmd, "/usr/local/bin/hook.sh");
        // Verify PostToolUseFailure hook
        assert!(parsed["hooks"]["PostToolUseFailure"].is_array());
        assert_eq!(parsed["hooks"]["PostToolUseFailure"][0]["matcher"], "Bash");
        let fail_cmd = parsed["hooks"]["PostToolUseFailure"][0]["hooks"][0]["command"]
            .as_str()
            .unwrap();
        assert_eq!(fail_cmd, "/usr/local/bin/failure-hook.sh");
        // Verify UserPromptSubmit hook
        assert!(parsed["hooks"]["UserPromptSubmit"].is_array());
        let prompt_cmd = parsed["hooks"]["UserPromptSubmit"][0]["hooks"][0]["command"]
            .as_str()
            .unwrap();
        assert_eq!(prompt_cmd, "/usr/local/bin/prompt-hook.sh");
    }

    #[test]
    fn test_merge_claude_settings_idempotent() {
        let temp_dir = tempfile::TempDir::new().unwrap();
        let settings_path = temp_dir.path().join("settings.json");
        let hook_path = "/home/user/.config/suvadu/hooks/claude-code-post-tool.sh";
        let failure_hook_path = "/home/user/.config/suvadu/hooks/claude-code-post-tool-failure.sh";
        let prompt_hook_path = "/home/user/.config/suvadu/hooks/claude-code-prompt.sh";

        // Start with empty settings
        std::fs::write(&settings_path, "{}").unwrap();

        // First merge should add all hooks
        let result = try_merge_claude_settings(
            &settings_path,
            hook_path,
            failure_hook_path,
            prompt_hook_path,
        )
        .unwrap();
        assert!(result);

        // Read and verify
        let content = std::fs::read_to_string(&settings_path).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&content).unwrap();
        assert_eq!(parsed["hooks"]["PostToolUse"].as_array().unwrap().len(), 1);
        assert_eq!(
            parsed["hooks"]["PostToolUseFailure"]
                .as_array()
                .unwrap()
                .len(),
            1
        );
        assert_eq!(
            parsed["hooks"]["UserPromptSubmit"]
                .as_array()
                .unwrap()
                .len(),
            1
        );

        // Second merge should detect existing and not duplicate
        let result2 = try_merge_claude_settings(
            &settings_path,
            hook_path,
            failure_hook_path,
            prompt_hook_path,
        )
        .unwrap();
        assert!(result2);

        let content2 = std::fs::read_to_string(&settings_path).unwrap();
        let parsed2: serde_json::Value = serde_json::from_str(&content2).unwrap();
        assert_eq!(parsed2["hooks"]["PostToolUse"].as_array().unwrap().len(), 1);
        assert_eq!(
            parsed2["hooks"]["PostToolUseFailure"]
                .as_array()
                .unwrap()
                .len(),
            1
        );
        assert_eq!(
            parsed2["hooks"]["UserPromptSubmit"]
                .as_array()
                .unwrap()
                .len(),
            1
        );
    }

    #[test]
    fn test_merge_claude_settings_preserves_existing() {
        let temp_dir = tempfile::TempDir::new().unwrap();
        let settings_path = temp_dir.path().join("settings.json");
        let hook_path = "/home/user/.config/suvadu/hooks/claude-code-post-tool.sh";
        let failure_hook_path = "/home/user/.config/suvadu/hooks/claude-code-post-tool-failure.sh";
        let prompt_hook_path = "/home/user/.config/suvadu/hooks/claude-code-prompt.sh";

        // Existing settings with other config
        let existing = serde_json::json!({
            "permissions": {"allow": ["Read", "Write"]},
            "hooks": {
                "PreToolUse": [{"matcher": "Write", "hooks": [{"type": "command", "command": "echo pre"}]}]
            }
        });
        std::fs::write(
            &settings_path,
            serde_json::to_string_pretty(&existing).unwrap(),
        )
        .unwrap();

        // Merge should add hooks without disturbing existing config
        let result = try_merge_claude_settings(
            &settings_path,
            hook_path,
            failure_hook_path,
            prompt_hook_path,
        )
        .unwrap();
        assert!(result);

        let content = std::fs::read_to_string(&settings_path).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&content).unwrap();

        // Existing config preserved
        assert!(parsed["permissions"]["allow"].is_array());
        assert!(parsed["hooks"]["PreToolUse"].is_array());
        // New hooks added
        assert_eq!(parsed["hooks"]["PostToolUse"].as_array().unwrap().len(), 1);
        assert_eq!(
            parsed["hooks"]["PostToolUseFailure"]
                .as_array()
                .unwrap()
                .len(),
            1
        );
    }

    #[test]
    fn test_claude_settings_snippet_structure() {
        let json = generate_claude_settings_snippet(
            "/path/to/hook.sh",
            "/path/to/failure-hook.sh",
            "/path/to/prompt-hook.sh",
        );
        let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();

        // Verify the JSON has the correct hook types
        let hooks = parsed.get("hooks").expect("Should have hooks key");
        assert!(
            hooks.get("PostToolUse").is_some(),
            "Should have PostToolUse hook type"
        );
        assert!(
            hooks.get("PostToolUseFailure").is_some(),
            "Should have PostToolUseFailure hook type"
        );
        assert!(
            hooks.get("UserPromptSubmit").is_some(),
            "Should have UserPromptSubmit hook type"
        );

        // Verify PostToolUse has Bash matcher
        let post_tool = hooks["PostToolUse"].as_array().unwrap();
        assert_eq!(post_tool.len(), 1);
        assert_eq!(post_tool[0]["matcher"], "Bash");

        // Verify PostToolUseFailure has Bash matcher
        let post_tool_fail = hooks["PostToolUseFailure"].as_array().unwrap();
        assert_eq!(post_tool_fail.len(), 1);
        assert_eq!(post_tool_fail[0]["matcher"], "Bash");

        // Verify UserPromptSubmit does NOT have a matcher (it fires for all prompts)
        let prompt_submit = hooks["UserPromptSubmit"].as_array().unwrap();
        assert_eq!(prompt_submit.len(), 1);
        assert!(
            prompt_submit[0].get("matcher").is_none(),
            "UserPromptSubmit should not have a matcher"
        );
    }

    #[test]
    fn test_merge_deduplicates_existing_duplicates() {
        let temp_dir = tempfile::TempDir::new().unwrap();
        let settings_path = temp_dir.path().join("settings.json");
        let hook_path = "/home/user/.config/suvadu/hooks/claude-code-post-tool.sh";
        let failure_hook_path = "/home/user/.config/suvadu/hooks/claude-code-post-tool-failure.sh";
        let prompt_hook_path = "/home/user/.config/suvadu/hooks/claude-code-prompt.sh";

        // Simulate 4 duplicate UserPromptSubmit entries (as seen in the wild)
        let settings = serde_json::json!({
            "hooks": {
                "PostToolUse": [{
                    "matcher": "Bash",
                    "hooks": [{"type": "command", "command": hook_path}]
                }],
                "UserPromptSubmit": [
                    {"hooks": [{"type": "command", "command": prompt_hook_path}]},
                    {"hooks": [{"type": "command", "command": prompt_hook_path}]},
                    {"hooks": [{"type": "command", "command": prompt_hook_path}]},
                    {"hooks": [{"type": "command", "command": prompt_hook_path}]}
                ]
            }
        });
        std::fs::write(
            &settings_path,
            serde_json::to_string_pretty(&settings).unwrap(),
        )
        .unwrap();

        // Merge should detect existing hooks, add missing failure hook, and deduplicate
        let result = try_merge_claude_settings(
            &settings_path,
            hook_path,
            failure_hook_path,
            prompt_hook_path,
        )
        .unwrap();
        assert!(result);

        let content = std::fs::read_to_string(&settings_path).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&content).unwrap();
        assert_eq!(parsed["hooks"]["PostToolUse"].as_array().unwrap().len(), 1);
        assert_eq!(
            parsed["hooks"]["UserPromptSubmit"]
                .as_array()
                .unwrap()
                .len(),
            1
        );
    }

    #[test]
    fn test_dedup_preserves_non_suvadu_hooks() {
        let mut settings = serde_json::json!({
            "hooks": {
                "UserPromptSubmit": [
                    {"hooks": [{"type": "command", "command": "/path/to/suvadu/hook.sh"}]},
                    {"hooks": [{"type": "command", "command": "/other/tool/hook.sh"}]},
                    {"hooks": [{"type": "command", "command": "/path/to/suvadu/hook.sh"}]}
                ]
            }
        });

        let changed = dedup_suvadu_hooks(&mut settings);
        assert!(changed);

        let arr = settings["hooks"]["UserPromptSubmit"].as_array().unwrap();
        assert_eq!(arr.len(), 2); // 1 suvadu + 1 other
                                  // First entry is suvadu, second is the other tool
        assert!(arr[0]["hooks"][0]["command"]
            .as_str()
            .unwrap()
            .contains("suvadu"));
        assert!(arr[1]["hooks"][0]["command"]
            .as_str()
            .unwrap()
            .contains("other"));
    }

    #[test]
    fn test_get_prompts_dir() {
        let result = get_prompts_dir();
        assert!(result.is_ok(), "get_prompts_dir should succeed");
        let path = result.unwrap();
        let path_str = path.to_string_lossy().to_string();
        // Should use directories crate path (contains suvadu) and end with prompts
        assert!(
            path_str.contains("suvadu"),
            "Prompts dir should be under suvadu data dir, got: {path_str}"
        );
        assert!(
            path_str.ends_with("prompts"),
            "Prompts dir should end with 'prompts', got: {path_str}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn test_codex_hook_script_resolves_stale_binary_from_path() {
        use std::os::unix::fs::PermissionsExt;

        let temp_dir = tempfile::TempDir::new().unwrap();
        let suv_path = temp_dir.path().join("suv");
        std::fs::write(&suv_path, "#!/bin/sh\nprintf '%s' \"$1\"\n").unwrap();
        std::fs::set_permissions(&suv_path, std::fs::Permissions::from_mode(0o700)).unwrap();

        let script = codex_hook_script(
            "/stale/bin/suv",
            "PostToolUse",
            "Records commands",
            "hook-codex",
        );
        let output = std::process::Command::new("/bin/bash")
            .arg("-c")
            .arg(script)
            .env("PATH", temp_dir.path())
            .output()
            .unwrap();

        assert!(output.status.success());
        assert_eq!(output.stdout, b"hook-codex");
    }

    #[test]
    fn test_merge_codex_hooks_preserves_existing_and_idempotent() {
        let temp_dir = tempfile::TempDir::new().unwrap();
        let hooks_json_path = temp_dir.path().join("hooks.json");
        let hook_path = "/home/u/.config/suvadu/hooks/codex-post-tool.sh";
        let prompt_hook_path = "/home/u/.config/suvadu/hooks/codex-prompt.sh";

        // Pre-existing codex hooks we must not clobber (e.g. synapse, plannotator)
        let existing = serde_json::json!({
            "hooks": {
                "SessionStart": [{
                    "matcher": "startup|resume|clear|compact",
                    "hooks": [{"type": "command", "command": "/home/u/.codex/hooks/synapse-session-start.sh"}]
                }],
                "Stop": [{
                    "hooks": [{"type": "command", "command": "/home/u/.local/bin/plannotator"}]
                }]
            }
        });
        std::fs::write(
            &hooks_json_path,
            serde_json::to_string_pretty(&existing).unwrap(),
        )
        .unwrap();

        // First merge adds suvadu PostToolUse + UserPromptSubmit
        let result = try_merge_codex_hooks(&hooks_json_path, hook_path, prompt_hook_path).unwrap();
        assert!(result);

        let parsed: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&hooks_json_path).unwrap()).unwrap();

        // Existing hooks preserved
        assert!(parsed["hooks"]["SessionStart"].is_array());
        assert_eq!(
            parsed["hooks"]["SessionStart"][0]["hooks"][0]["command"],
            "/home/u/.codex/hooks/synapse-session-start.sh"
        );
        assert!(parsed["hooks"]["Stop"].is_array());

        // suvadu hooks added
        assert_eq!(parsed["hooks"]["PostToolUse"][0]["matcher"], "^Bash$");
        assert_eq!(
            parsed["hooks"]["PostToolUse"][0]["hooks"][0]["command"],
            hook_path
        );
        assert!(parsed["hooks"]["UserPromptSubmit"].is_array());
        assert_eq!(
            parsed["hooks"]["UserPromptSubmit"][0]["hooks"][0]["command"],
            prompt_hook_path
        );

        // Second merge is idempotent — no duplicate suvadu groups
        let result2 = try_merge_codex_hooks(&hooks_json_path, hook_path, prompt_hook_path).unwrap();
        assert!(result2);
        let parsed2: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&hooks_json_path).unwrap()).unwrap();
        assert_eq!(parsed2["hooks"]["PostToolUse"].as_array().unwrap().len(), 1);
        assert_eq!(
            parsed2["hooks"]["UserPromptSubmit"]
                .as_array()
                .unwrap()
                .len(),
            1
        );
        // Still no clobbering
        assert!(parsed2["hooks"]["SessionStart"].is_array());
        assert!(parsed2["hooks"]["Stop"].is_array());
    }

    #[test]
    fn test_configure_codex_mcp_idempotent() {
        // Warm the process-wide `PROJECT_DIRS` `LazyLock` (src/util/mod.rs) under the
        // real HOME before we redirect it below. Otherwise, if another test running in
        // parallel is the first to call `project_dirs()` while HOME points at our temp
        // dir, this test's temp path gets permanently cached for the rest of the
        // process, breaking unrelated tests.
        let _ = crate::util::project_dirs();

        let temp_dir = tempfile::TempDir::new().unwrap();
        let home_backup = std::env::var("HOME").ok();
        std::env::set_var("HOME", temp_dir.path());

        // First run creates config.toml with our server
        let first = try_configure_codex_mcp("/usr/local/bin/suv").unwrap();
        assert!(first);

        let config_path = temp_dir.path().join(".codex").join("config.toml");
        let content = std::fs::read_to_string(&config_path).unwrap();
        assert!(content.contains("mcp_servers"));
        assert!(content.contains("mcp-serve"));

        // The registered server is a STDIO server pointing at `suv mcp-serve`,
        // exactly the launcher Codex's MCP client needs.
        let parsed: toml::Value = toml::from_str(&content).unwrap();
        let server = &parsed["mcp_servers"]["suvadu"];
        assert_eq!(server["command"].as_str(), Some("/usr/local/bin/suv"));
        let args = server["args"]
            .as_array()
            .map(|a| a.iter().map(|v| v.as_str().unwrap()).collect::<Vec<_>>())
            .unwrap();
        assert_eq!(args, vec!["mcp-serve"]);

        let mut stale: toml::Value = toml::from_str(&content).unwrap();
        let server = stale["mcp_servers"]["suvadu"].as_table_mut().unwrap();
        server.insert(
            "command".to_string(),
            toml::Value::String("/stale/bin/suv".to_string()),
        );
        server.insert("startup_timeout_sec".to_string(), toml::Value::Integer(15));
        std::fs::write(&config_path, toml::to_string_pretty(&stale).unwrap()).unwrap();

        // Re-running init refreshes Suvadu's launcher without replacing other settings.
        let second = try_configure_codex_mcp("/opt/homebrew/bin/suv").unwrap();
        assert!(second);
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

        try_configure_codex_mcp("/new/bin/suv").unwrap();
        let refreshed = std::fs::read_to_string(&config_path).unwrap();
        assert_eq!(
            refreshed,
            hand_written.replace("/old/bin/suv", "/new/bin/suv")
        );

        let mtime_before_noop_rerun = std::fs::metadata(&config_path).unwrap().modified().unwrap();
        try_configure_codex_mcp("/new/bin/suv").unwrap();
        assert_eq!(std::fs::read_to_string(&config_path).unwrap(), refreshed);
        assert_eq!(
            std::fs::metadata(&config_path).unwrap().modified().unwrap(),
            mtime_before_noop_rerun
        );

        if let Some(h) = home_backup {
            std::env::set_var("HOME", h);
        } else {
            std::env::remove_var("HOME");
        }
    }

    #[test]
    fn test_codex_exit_code_helper() {
        // No exit code or error exposed → unknown (None), not a guessed 0
        let unknown = serde_json::json!({
            "session_id": "thr_1",
            "cwd": "/repo",
            "tool_name": "Bash",
            "tool_input": {"command": "cargo build"},
            "tool_response": {"stdout": "done"}
        });
        assert_eq!(codex_exit_code(&unknown), None);

        // Explicit failure exit code wins
        let explicit = serde_json::json!({
            "session_id": "thr_1",
            "cwd": "/repo",
            "tool_name": "Bash",
            "tool_input": {"command": "failing"},
            "tool_response": {"exit_code": 2}
        });
        assert_eq!(codex_exit_code(&explicit), Some(2));

        // Failure reported via error string → parsed exit code
        let errored = serde_json::json!({
            "session_id": "thr_1",
            "cwd": "/repo",
            "tool_name": "Bash",
            "tool_input": {"command": "make"},
            "tool_response": {"error": "Command exited with non-zero status code 127"}
        });
        assert_eq!(codex_exit_code(&errored), Some(127));

        // Error present but code not parseable → default 1
        let generic_error = serde_json::json!({
            "session_id": "thr_1",
            "tool_name": "Bash",
            "tool_input": {"command": "make"},
            "tool_response": {"error": "Something broke"}
        });
        assert_eq!(codex_exit_code(&generic_error), Some(1));

        // Error key present but explicitly null (e.g. Option<String> serialized without
        // skip_serializing_if) → still unknown (None), not a guessed failure
        let null_error = serde_json::json!({
            "session_id": "thr_1",
            "tool_name": "Bash",
            "tool_input": {"command": "cargo build"},
            "tool_response": {"stdout": "done", "error": null}
        });
        assert_eq!(codex_exit_code(&null_error), None);
    }

    /// Real `PostToolUse` payloads from codex-cli 0.148 (captured from a live
    /// session): `tool_response` is a plain string, and success and failure are
    /// indistinguishable, so both must record an unknown exit code.
    #[test]
    fn test_codex_exit_code_real_codex_payload_shape() {
        let success = serde_json::json!({
            "session_id": "01a01ade-c43e-76f1-963f-1202e7c394a0",
            "turn_id": "01a01ade-c524-7fe0-b7c6-41b998616b9e",
            "cwd": "/Users/dev/code/suvadu",
            "hook_event_name": "PostToolUse",
            "model": "gpt-5.6-sol",
            "permission_mode": "default",
            "tool_name": "Bash",
            "tool_input": {"command": "echo hello"},
            "tool_response": "hello\n",
            "tool_use_id": "exec-837af453-534c-47b5-9a47-fb3d9f65b912"
        });
        let failure = serde_json::json!({
            "session_id": "01a01adf-4321-7713-aab3-084b2edc6f0f",
            "turn_id": "01a01adf-4400-7000-8000-084b2edc6f0f",
            "cwd": "/Users/dev/code/suvadu",
            "hook_event_name": "PostToolUse",
            "model": "gpt-5.6-sol",
            "permission_mode": "default",
            "tool_name": "Bash",
            "tool_input": {"command": "ls /nonexistent"},
            "tool_response": "ls: /nonexistent: No such file or directory\n",
            "tool_use_id": "exec-4e8e6741-361a-4b97-bedf-9ed151cfc939"
        });

        assert_eq!(codex_exit_code(&success), None);
        assert_eq!(codex_exit_code(&failure), None);

        for event in [&success, &failure] {
            assert_eq!(event["tool_name"], "Bash");
            assert!(!event["tool_input"]["command"].as_str().unwrap().is_empty());
            assert!(event["cwd"].as_str().unwrap().starts_with('/'));
            assert!(is_valid_session_id(event["session_id"].as_str().unwrap()));
        }
    }

    #[test]
    fn test_tool_response_exit_code_accepts_all_spellings() {
        for key in ["exit_code", "exitCode", "status_code"] {
            let event = serde_json::json!({"tool_response": {key: 3}});
            assert_eq!(tool_response_exit_code(&event), Some(3), "key {key}");
        }

        // Absent tool_response → None, which the Claude Code hook defaults to 0
        assert_eq!(
            tool_response_exit_code(&serde_json::json!({"tool_name": "Bash"})),
            None
        );
    }
}
