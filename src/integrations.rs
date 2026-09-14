//! IDE and agent integration hooks for Suvadu.
//!
//! Contains Claude Code hook handlers, IDE init routines (Cursor, Antigravity),
//! and helpers for merging settings into `~/.claude/settings.json`.

pub mod agent_hook;
pub mod codex;
pub mod registry;

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use crate::util::atomic_write;

/// Maximum bytes to read from stdin for hook input (1 MB).
const MAX_HOOK_INPUT_BYTES: u64 = 1_048_576;

/// Handle `PostToolUse` hook from Claude Code — reads JSON event from stdin and records the command.
pub fn handle_hook_claude_code() -> Result<(), Box<dyn std::error::Error>> {
    use std::io::Read;

    let mut input = String::new();
    std::io::stdin()
        .take(MAX_HOOK_INPUT_BYTES)
        .read_to_string(&mut input)?;

    let event: serde_json::Value = serde_json::from_str(&input)?;

    // Only process Bash tool calls
    let tool_name = event
        .get("tool_name")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("");
    if tool_name != "Bash" {
        return Ok(());
    }

    // Extract the command
    let command = event
        .get("tool_input")
        .and_then(|ti| ti.get("command"))
        .and_then(serde_json::Value::as_str)
        .unwrap_or("");
    if command.is_empty() {
        return Ok(());
    }

    // Extract working directory
    let cwd = event
        .get("cwd")
        .and_then(serde_json::Value::as_str)
        .unwrap_or(".");

    // PostToolUse only fires on successful commands (exit 0).
    // Try to read an explicit exit_code from tool_response for forward-compat,
    // but default to 0 since this hook only fires on success.
    let exit_code = event
        .get("tool_response")
        .and_then(|tr| {
            tr.get("exit_code")
                .or_else(|| tr.get("exitCode"))
                .or_else(|| tr.get("status_code"))
        })
        .and_then(serde_json::Value::as_i64)
        .and_then(|ec| i32::try_from(ec).ok())
        .or(Some(0));

    // Use Claude Code's session_id, prefixed to avoid collision with zsh sessions
    let native_session_id = event
        .get("session_id")
        .and_then(serde_json::Value::as_str)
        .filter(|s| is_valid_session_id(s));
    let session_id = native_session_id.map_or_else(
        || format!("claude-{}", uuid::Uuid::new_v4()),
        |s| format!("claude-{s}"),
    );

    let now = chrono::Utc::now().timestamp_millis();

    // Read cached prompt for this session (set by UserPromptSubmit hook)
    let context = claude_command_context(&event, native_session_id, &session_id);

    crate::commands::entry::handle_add_with_context(crate::commands::entry::AddParams {
        session_id,
        command: command.to_string(),
        cwd: cwd.to_string(),
        exit_code,
        started_at: now,
        ended_at: now,
        executor_type: Some("agent".to_string()),
        executor: Some("claude-code".to_string()),
        context,
    })
}

/// Handle `UserPromptSubmit` hook from Claude Code — caches the prompt text
pub fn handle_hook_claude_prompt() -> Result<(), Box<dyn std::error::Error>> {
    use std::io::Read;

    let mut input = String::new();
    std::io::stdin()
        .take(MAX_HOOK_INPUT_BYTES)
        .read_to_string(&mut input)?;

    let event: serde_json::Value = serde_json::from_str(&input)?;

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

    if crate::config::is_paused() {
        return Ok(());
    }
    let cfg = crate::config::load_config_cached()?;
    if !cfg.enabled {
        return Ok(());
    }

    // Store prompt in cache file (atomic write to avoid corruption on crash)
    let turn_id = event
        .get("prompt_id")
        .and_then(serde_json::Value::as_str)
        .filter(|id| is_valid_session_id(id));
    let prompt_file = claude_prompt_path(session_id, turn_id)?;
    std::fs::create_dir_all(prompt_file.parent().ok_or("Invalid prompt cache path")?)?;
    let safe = if cfg.redaction.enabled {
        crate::redact::redact_secrets_with_extra(prompt, &cfg.redaction.extra_patterns)
    } else {
        prompt.to_owned()
    };
    let truncated = crate::util::truncate_str(&safe, cfg.agent.prompt_capture_max_chars, "...");
    atomic_write(&prompt_file, &truncated)?;

    // Restrict prompt cache file to owner-only (contains user prompts)
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&prompt_file, std::fs::Permissions::from_mode(0o600));
    }

    Ok(())
}

/// Handle the `hook-opencode-prompt` command — caches an `OpenCode` prompt.
///
/// The plugin (`OPENCODE_PLUGIN_SCRIPT` below) passes the session ID and
/// `input.directory` as `--session-id`/`--directory` and pipes the raw
/// prompt text on stdin, since it has no access to Suvadu's config or
/// redaction logic itself. This routes the prompt through the exact same
/// redact-then-truncate pipeline as the Claude Code and Codex prompt hooks
/// above (`cfg.redaction`, `cfg.agent.prompt_capture_max_chars`) so the
/// cached text an operator sees matches what's stored on import, and so
/// `reconcile_agent_command_turns`'s exact-text match against the imported
/// `prompt` `AiEvent` (which goes through the same policy via
/// `sanitize_event`) still succeeds for prompts that get redacted or
/// truncated. Uses `load_config_for_dir(directory)` rather than the global
/// config, matching `import_native_opencode`'s per-project policy for the
/// same directory — a project `.suvadu.toml` that changes redaction or
/// `prompt_capture_max_chars` would otherwise reintroduce a cache/import
/// mismatch, or leave a secret unredacted in the cache when the project
/// (unlike global config) turns redaction on.
pub fn handle_hook_opencode_prompt(
    session_id: &str,
    directory: Option<&str>,
) -> Result<(), Box<dyn std::error::Error>> {
    use std::io::Read;

    let Some(directory) = directory.filter(|directory| !directory.is_empty()) else {
        return Ok(());
    };
    if !is_valid_session_id(session_id) {
        return Ok(());
    }

    let mut prompt = String::new();
    std::io::stdin()
        .take(MAX_HOOK_INPUT_BYTES)
        .read_to_string(&mut prompt)?;
    if prompt.is_empty() {
        return Ok(());
    }

    if crate::config::is_paused() {
        return Ok(());
    }
    let cfg = crate::config::load_config_for_dir(Path::new(directory))?;
    if !cfg.enabled {
        return Ok(());
    }

    // Hashed before any redaction/truncation, so a command's own (possibly
    // stricter, nested-directory) config can re-redact this cached prompt
    // for display without breaking `reconcile_agent_command_turns`'s match
    // against the session-wide imported prompt event, which is sanitized
    // under this same directory's policy and hashes the same raw text.
    // Keyed (`keyed_text_hash`, not a bare hash) so the fingerprint alone
    // can't be dictionary-guessed offline by anyone who can read this file.
    let text_hash = crate::util::keyed_text_hash(&prompt);

    let prompts_dir = get_prompts_dir()?;
    std::fs::create_dir_all(&prompts_dir)?;
    let prompt_file = prompts_dir.join(format!("opencode-{session_id}.prompt"));
    let hash_file = prompts_dir.join(format!("opencode-{session_id}.prompt.hash"));
    let safe = if cfg.redaction.enabled {
        crate::redact::redact_secrets_with_extra(&prompt, &cfg.redaction.extra_patterns)
    } else {
        prompt
    };
    let truncated = crate::util::truncate_str(&safe, cfg.agent.prompt_capture_max_chars, "...");
    atomic_write(&prompt_file, &truncated)?;
    if let Some(text_hash) = &text_hash {
        atomic_write(&hash_file, text_hash)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(&hash_file, std::fs::Permissions::from_mode(0o600));
        }
    }

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&prompt_file, std::fs::Permissions::from_mode(0o600));
    }

    Ok(())
}

/// Handle `PostToolUseFailure` hook from Claude Code — records failed commands.
///
/// This hook fires when a Bash command exits with a non-zero status code.
/// The payload has an `error` string (no `tool_response`) from which we
/// parse the exit code.
pub fn handle_hook_claude_code_failure() -> Result<(), Box<dyn std::error::Error>> {
    use std::io::Read;

    let mut input = String::new();
    std::io::stdin()
        .take(MAX_HOOK_INPUT_BYTES)
        .read_to_string(&mut input)?;

    let event: serde_json::Value = serde_json::from_str(&input)?;

    // Only process Bash tool calls
    let tool_name = event
        .get("tool_name")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("");
    if tool_name != "Bash" {
        return Ok(());
    }

    // Extract the command
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

    // Parse exit code from error string, e.g. "non-zero status code 1"
    let exit_code = event
        .get("error")
        .and_then(serde_json::Value::as_str)
        .and_then(parse_exit_code_from_error)
        .or(Some(1)); // default to 1 if we can't parse

    let native_session_id = event
        .get("session_id")
        .and_then(serde_json::Value::as_str)
        .filter(|s| is_valid_session_id(s));
    let session_id = native_session_id.map_or_else(
        || format!("claude-{}", uuid::Uuid::new_v4()),
        |s| format!("claude-{s}"),
    );

    let now = chrono::Utc::now().timestamp_millis();

    let context = claude_command_context(&event, native_session_id, &session_id);

    crate::commands::entry::handle_add_with_context(crate::commands::entry::AddParams {
        session_id,
        command: command.to_string(),
        cwd: cwd.to_string(),
        exit_code,
        started_at: now,
        ended_at: now,
        executor_type: Some("agent".to_string()),
        executor: Some("claude-code".to_string()),
        context,
    })
}

/// Reconcile Claude Code's native transcript at the end of each turn/session.
pub fn handle_hook_claude_session() -> Result<(), Box<dyn std::error::Error>> {
    use std::io::Read;

    let mut input = String::new();
    std::io::stdin()
        .take(MAX_HOOK_INPUT_BYTES + 1)
        .read_to_string(&mut input)?;
    if input.is_empty() {
        return Ok(());
    }
    if input.len() as u64 > MAX_HOOK_INPUT_BYTES {
        return Err("Claude hook input exceeds 1 MB".into());
    }
    let event: serde_json::Value = serde_json::from_str(&input)?;
    if !matches!(
        event["hook_event_name"].as_str(),
        Some("Stop" | "SessionEnd")
    ) {
        return Ok(());
    }
    let Some(session_id) = event["session_id"]
        .as_str()
        .filter(|id| is_valid_session_id(id))
    else {
        return Ok(());
    };
    let Some(path) = event["transcript_path"]
        .as_str()
        .filter(|path| !path.is_empty())
    else {
        return Ok(());
    };
    match crate::commands::agent_session::import_native_claude(Path::new(path), Some(session_id)) {
        Ok(result) if result["has_more"] == true => {
            eprintln!("suvadu: more transcript data remains; a later hook or suv agent import-session will continue");
        }
        Ok(_) => {}
        Err(error) => eprintln!("suvadu: session capture: {error}"),
    }
    Ok(())
}

/// `OpenCode` sends a full session's message history per call (unlike the
/// other hooks' small single-event payloads), so this uses its own larger
/// cap instead of the shared `MAX_HOOK_INPUT_BYTES`.
const MAX_OPENCODE_HOOK_INPUT_BYTES: u64 = 16 * 1024 * 1024;

pub fn handle_hook_opencode_session(
    session_id: &str,
    directory: Option<&str>,
) -> Result<(), Box<dyn std::error::Error>> {
    use std::io::Read;

    let Some(directory) = directory.filter(|directory| !directory.is_empty()) else {
        return Ok(());
    };
    if !is_valid_session_id(session_id) {
        return Ok(());
    }
    let mut input = String::new();
    std::io::stdin()
        .take(MAX_OPENCODE_HOOK_INPUT_BYTES + 1)
        .read_to_string(&mut input)?;
    if input.is_empty() {
        return Ok(());
    }
    if input.len() as u64 > MAX_OPENCODE_HOOK_INPUT_BYTES {
        return Err("OpenCode hook input exceeds 16 MB".into());
    }
    match crate::commands::agent_session::import_native_opencode(session_id, directory, &input) {
        Ok(_) => {}
        Err(error) => eprintln!("suvadu: OpenCode session capture: {error}"),
    }
    Ok(())
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

fn claude_prompt_path(
    native_session_id: &str,
    turn_id: Option<&str>,
) -> Result<PathBuf, Box<dyn std::error::Error>> {
    let root = get_prompts_dir()?;
    Ok(turn_id.map_or_else(
        || root.join(format!("claude-{native_session_id}.prompt")),
        |turn| {
            root.join("claude")
                .join(native_session_id)
                .join(format!("{turn}.prompt"))
        },
    ))
}

fn claude_command_context(
    event: &serde_json::Value,
    native_session_id: Option<&str>,
    prefixed_session_id: &str,
) -> Option<HashMap<String, String>> {
    let turn = event
        .get("prompt_id")
        .and_then(serde_json::Value::as_str)
        .filter(|id| is_valid_session_id(id));
    let prompt = native_session_id
        .and_then(|session| claude_prompt_path(session, turn).ok())
        .and_then(|path| std::fs::read_to_string(path).ok())
        .or_else(|| get_cached_prompt(prefixed_session_id));
    if prompt.is_none() && turn.is_none() {
        return None;
    }
    let mut context = HashMap::new();
    if let Some(prompt) = prompt.filter(|prompt| !prompt.is_empty()) {
        context.insert("agent_prompt".into(), prompt);
    }
    if let Some(turn) = turn {
        context.insert("agent_turn_id".into(), turn.into());
    }
    Some(context)
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
pub fn shell_escape(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
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
    let script = agent_hook::script(&bin_path, "hook-claude-code", "claude-code");
    crate::util::atomic_write_with_mode(&hook_script_path, &script, 0o700)?;

    // Write the PostToolUseFailure hook script
    let failure_hook_path = hooks_dir.join("claude-code-post-tool-failure.sh");
    let failure_script = agent_hook::script(&bin_path, "hook-claude-code-failure", "claude-code");
    crate::util::atomic_write_with_mode(&failure_hook_path, &failure_script, 0o700)?;

    // Write the UserPromptSubmit hook script
    let prompt_hook_path = hooks_dir.join("claude-code-prompt.sh");
    let prompt_script = agent_hook::script(&bin_path, "hook-claude-prompt", "claude-code");
    crate::util::atomic_write_with_mode(&prompt_hook_path, &prompt_script, 0o700)?;

    // Write the Stop/SessionEnd transcript reconciliation hook.
    let session_hook_path = hooks_dir.join("claude-code-session.sh");
    let session_script = agent_hook::script(&bin_path, "hook-claude-session", "claude-code");
    crate::util::atomic_write_with_mode(&session_hook_path, &session_script, 0o700)?;

    let hook_path_str = hook_script_path.to_string_lossy().to_string();
    let failure_hook_path_str = failure_hook_path.to_string_lossy().to_string();
    let prompt_hook_path_str = prompt_hook_path.to_string_lossy().to_string();
    let session_hook_path_str = session_hook_path.to_string_lossy().to_string();

    // Try auto-merge into ~/.claude/settings.json
    let settings_path = PathBuf::from(&home).join(".claude").join("settings.json");

    let auto_configured = try_merge_claude_settings(
        &settings_path,
        &hook_path_str,
        &failure_hook_path_str,
        &prompt_hook_path_str,
        &session_hook_path_str,
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
    println!("  {session_hook_path_str}");
    println!();

    if matches!(auto_configured, Ok(true)) {
        println!(
            "{green}✓{r} Settings auto-configured: {}",
            settings_path.display()
        );
        println!();
        println!("Relaunch Claude Code to activate the updated hooks.");
    } else {
        println!("Add this to ~/.claude/settings.json:");
        println!();
        println!(
            "{}",
            generate_claude_settings_snippet(
                &hook_path_str,
                &failure_hook_path_str,
                &prompt_hook_path_str,
                &session_hook_path_str,
            )
        );
        println!();
        println!("Then relaunch Claude Code to activate the updated hooks.");
    }
    println!("For the VS Code extension, fully quit and reopen VS Code.");

    // Auto-configure MCP server
    let mcp_configured = try_configure_claude_mcp(&bin_path);
    if matches!(mcp_configured, Ok(true)) {
        println!("{green}\u{2713}{r} MCP server auto-configured in ~/.claude.json");
        println!("  AI agents can now query your shell history via MCP.");
    }

    if matches!(try_install_builtin_skills(), Ok(true)) {
        println!("{green}\u{2713}{r} Installed the suvadu-session-memory skill");
        println!(
            "  Reinforces preferring Suvadu's own session summaries over a generic memory note."
        );
    }

    println!();
    println!(
        "Session prompts, responses, models, and reported tokens are captured at Stop/SessionEnd."
    );
    println!("View sessions: {cyan}suv sessions{r}");
    println!("Verify with: {cyan}suv search --executor agent{r}");
    print_post_install_tips(cyan, r, true, true);

    Ok(())
}

/// Seed/refresh suvadu-owned builtin skills and materialize them into
/// Claude Code's native format immediately, so they're available after
/// the very first `suv init claude-code`, not only after a later manual
/// `suv skills sync`. Best-effort: any failure here (config load,
/// database, sync) must not fail `suv init claude-code` itself, matching
/// how the MCP-registration step just above already handles its own
/// failure silently.
fn try_install_builtin_skills() -> Result<bool, Box<dyn std::error::Error>> {
    let config = crate::config::load_config_cached()?;
    let repo = crate::repository::Repository::init()?;
    if !crate::skills_builtin::ensure_installed(&repo, &config)? {
        return Ok(false);
    }
    let cwd = std::env::current_dir()?;
    crate::skills_sync::sync(&repo, &[crate::cli::SyncTarget::ClaudeCode], &cwd, false)?;
    Ok(true)
}

/// Generate the JSON snippet for Claude Code settings.
pub fn generate_claude_settings_snippet(
    hook_path: &str,
    failure_hook_path: &str,
    prompt_hook_path: &str,
    session_hook_path: &str,
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
            }],
            "Stop": [{
                "hooks": [{
                    "type": "command",
                    "command": session_hook_path,
                    "timeout": 10
                }]
            }],
            "SessionEnd": [{
                "hooks": [{
                    "type": "command",
                    "command": session_hook_path,
                    "timeout": 10
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
    for key in [
        "PostToolUse",
        "PostToolUseFailure",
        "UserPromptSubmit",
        "Stop",
        "SessionEnd",
    ] {
        let Some(arr) = settings
            .get_mut("hooks")
            .and_then(|h| h.get_mut(key))
            .and_then(serde_json::Value::as_array_mut)
        else {
            continue;
        };
        let mut seen_suvadu = false;
        arr.retain_mut(|group| {
            let Some(handlers) = group
                .get_mut("hooks")
                .and_then(serde_json::Value::as_array_mut)
            else {
                return true;
            };
            handlers.retain(|handler| {
                let is_suvadu = handler
                    .get("command")
                    .and_then(serde_json::Value::as_str)
                    .is_some_and(is_suvadu_hook_command);
                if !is_suvadu {
                    return true;
                }
                if seen_suvadu {
                    changed = true;
                    false
                } else {
                    seen_suvadu = true;
                    true
                }
            });
            if handlers.is_empty() {
                changed = true;
                false
            } else {
                true
            }
        });
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

fn add_claude_session_hooks(
    hooks_obj: &mut serde_json::Map<String, serde_json::Value>,
    session_hook_path: &str,
) -> Result<bool, Box<dyn std::error::Error>> {
    let mut added = false;
    for hook_type in ["Stop", "SessionEnd"] {
        if !has_suvadu_hook_in_obj(hooks_obj, hook_type) {
            add_hook_entry(
                hooks_obj,
                hook_type,
                serde_json::json!({
                    "hooks": [{
                        "type": "command",
                        "command": session_hook_path,
                        "timeout": 10
                    }]
                }),
            )?;
            added = true;
        }
    }
    Ok(added)
}

/// Try to merge Suvadu hooks into an existing Claude settings file.
pub fn try_merge_claude_settings(
    settings_path: &Path,
    hook_path: &str,
    failure_hook_path: &str,
    prompt_hook_path: &str,
    session_hook_path: &str,
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

        let added_session = add_claude_session_hooks(hooks_obj, session_hook_path)?;

        // Also deduplicate
        let deduped = dedup_suvadu_hooks(&mut settings);
        if added_failure || added_prompt || added_session || deduped {
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

    add_claude_session_hooks(hooks_obj, session_hook_path)?;

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
/// Keeps the existing `tool.execute.after` bash-command forwarding
/// unchanged (`OpenCode`'s equivalent of Claude Code's live `PostToolUse`
/// hook), and keeps the `message.updated`/`message.part.updated` prompt
/// cache (feeds `entries.context["agent_prompt"]` via `suv add`'s own
/// prompt-cache read -- a separate mechanism from the import below that
/// `reconcile_agent_command_turns` depends on to group `OpenCode` commands
/// with their turn). That cache is written via `suv hook-opencode-prompt`
/// rather than a direct file write, so it goes through the same
/// redact-then-truncate policy as the imported `prompt` `AiEvent` below --
/// otherwise `reconcile_agent_command_turns`'s exact-text match against
/// that event would miss whenever a prompt got redacted or truncated
/// differently on each side. On `session.idle` -- and once more on plugin
/// `dispose()` as a best-effort final flush -- it fetches this session's
/// full message history via `OpenCode`'s own documented API and hands it to
/// `suv hook-opencode-session`, which does the incremental import.
///
/// The default export must be the bare `async (input) => Hooks` factory
/// function below, not an `{ id, setup }` (or `{ id, effect }`) object --
/// that shape belongs to a mismatched pre-release `opencode` CLI build
/// (self-reported v2.0.1/v2.0.3) seen earlier in development, which had
/// drifted ahead of the published SDK and must not be reintroduced. The
/// bare factory function is correct for the CLI version suvadu actually
/// targets, matching the published `@opencode-ai/plugin@1.18.30` npm
/// types exactly, confirmed empirically end-to-end against a real local
/// `opencode` v1.18.30 install (see the test below for the full story).
const OPENCODE_PLUGIN_SCRIPT: &str = r#"// Suvadu — OpenCode Integration Plugin
// Records AI-executed commands and captures session history
// Generated by: suv init opencode
import { spawnSync } from "child_process";

// Track user message IDs so we can match prompts to the right messages
const userMessageIDs = new Set();

// Caches the prompt via `suv hook-opencode-prompt`, not a direct file write:
// only the Rust side has the user's redaction config and prompt-length
// limit, so this is the same redact-then-truncate path every other agent
// integration's prompt cache goes through (and what the session-history
// import below compares against when grouping commands under a prompt).
// `directory` is passed through so both sides resolve the same
// project-local config (a .suvadu.toml can override redaction/length),
// not just the global one.
function cachePrompt(sessionID, prompt, directory) {
  try {
    spawnSync(
      "suv",
      ["hook-opencode-prompt", "--session-id", sessionID, "--directory", directory],
      { input: prompt, stdio: ["pipe", "ignore", "inherit"], timeout: 5000 }
    );
  } catch {
    // Best-effort — don't break OpenCode if suvadu fails
  }
}

async function importSession(client, sessionID, directory) {
  try {
    const response = await client.session.messages({ path: { id: sessionID } });
    const messages = response.data ?? response;
    spawnSync(
      "suv",
      ["hook-opencode-session", "--session-id", sessionID, "--directory", directory],
      { input: JSON.stringify(messages), stdio: ["pipe", "ignore", "inherit"], timeout: 10000 }
    );
  } catch {
    // Best-effort — don't break OpenCode if suvadu fails
  }
}

export default async (input) => {
  // Tracks the most recently active session so `dispose()` (which fires on
  // plugin unload/process exit with no event of its own) has something to
  // flush. Updated from both hooks below, so a session that never reaches
  // session.idle before exit is still captured on a best-effort basis.
  let lastSessionID = null;

  return {
    event: async (eventInput) => {
      const evt = eventInput?.event;
      if (!evt) return;

      // Track user messages so agent_prompt context can be attached to
      // live-forwarded shell commands via suv add's own prompt-cache read.
      if (evt.type === "message.updated") {
        const info = evt.properties?.info;
        if (info?.role === "user" && info?.id) {
          userMessageIDs.add(info.id);
        }
      }
      if (evt.type === "message.part.updated") {
        const part = evt.properties?.part;
        if (part?.type === "text" && part?.text && part?.messageID && userMessageIDs.has(part.messageID)) {
          const sessionID = part.sessionID || "unknown";
          cachePrompt(sessionID, part.text, input.directory);
          userMessageIDs.delete(part.messageID);
        }
      }

      if (evt.type === "session.idle" && evt.properties?.sessionID) {
        lastSessionID = evt.properties.sessionID;
        await importSession(input.client, lastSessionID, input.directory);
      }
    },

    "tool.execute.after": async (toolInput, output) => {
      if (toolInput.sessionID) {
        lastSessionID = toolInput.sessionID;
      }
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

    dispose: async () => {
      // Best-effort final flush for a session that ends without a prior
      // session.idle (e.g. the process exits mid-turn). `input` is in
      // closure scope here same as in the other hooks above; the only
      // real gap is not knowing *which* session was active, which
      // `lastSessionID` tracks.
      if (lastSessionID) {
        await importSession(input.client, lastSessionID, input.directory);
      }
    },
  };
};
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
/// `OpenCode` plugin auto-discovery from `~/.opencode/plugins/*.{ts,js}` alone
/// was found unreliable against a real local install (the plugin loaded only
/// once its directory was also explicitly listed in the `"plugin"` array of
/// `~/.config/opencode/opencode.jsonc`). This writes the plugin script and
/// then ensures both that config registration and MCP server registration,
/// via `try_configure_opencode`, rather than relying on auto-discovery.
pub fn handle_init_opencode() -> Result<(), Box<dyn std::error::Error>> {
    let home = std::env::var("HOME")?;
    let current_exe = std::env::current_exe()?;
    let bin_path = current_exe.to_string_lossy().to_string();

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
    let yellow = if color { "\x1b[33m" } else { "" };
    let cyan = if color { "\x1b[36m" } else { "" };

    println!("{b}Suvadu \u{2014} OpenCode Integration{r}");
    println!();
    println!(
        "{green}\u{2713}{r} Plugin installed: {}",
        plugin_path.display()
    );

    match try_configure_opencode(&plugins_dir, &bin_path) {
        Ok(()) => {
            println!("{green}\u{2713}{r} Registered plugin directory in opencode.jsonc");
            println!("{green}\u{2713}{r} MCP server auto-configured in opencode.jsonc");
            println!("  AI agents in OpenCode can now query your shell history via MCP.");
        }
        Err(e) => {
            println!("{yellow}\u{26a0}{r} Could not update opencode.jsonc automatically: {e}");
            println!("  Add this to ~/.config/opencode/opencode.jsonc yourself:");
            println!(
                "  {{\"plugin\": [\"{}\"], \"mcp\": {{\"suvadu\": {{\"type\": \"local\", \"command\": [\"{bin_path}\", \"mcp-serve\"]}}}}}}",
                plugins_dir.display()
            );
        }
    }

    println!();
    println!("Commands executed by OpenCode will be recorded with executor=opencode.");
    println!("Prompts, responses, model, and token usage are captured when a session goes idle.");
    println!();
    println!("Verify with: {cyan}suv search --executor opencode{r}");
    println!("            {cyan}suv agent sessions{r}");
    print_post_install_tips(cyan, r, true, false);

    Ok(())
}

/// Auto-configure `OpenCode`'s `~/.config/opencode/opencode.jsonc`: adds
/// `plugins_dir` to the top-level `"plugin"` array, and registers `suvadu`
/// as a local MCP server under `"mcp"` (`OpenCode`'s own `"mcp".<name>` shape,
/// distinct from Claude Code/Cursor's `"mcpServers"` and Codex's
/// `[mcp_servers.*]`). Each is only added if not already present -- an
/// existing `"mcp".suvadu` entry is left untouched rather than overwritten,
/// so a user's own customization (e.g. `"enabled": false`) survives a
/// rerun. Errors (e.g. an existing config containing JSONC comments, which
/// this plain-JSON parse can't round-trip safely) are surfaced to the
/// caller rather than risking corruption of a file that holds the user's
/// other `OpenCode` settings too.
fn try_configure_opencode(
    plugins_dir: &Path,
    bin_path: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    let home = std::env::var("HOME")?;
    let config_dir = PathBuf::from(&home).join(".config").join("opencode");
    std::fs::create_dir_all(&config_dir)?;
    let config_path = config_dir.join("opencode.jsonc");

    let mut config: serde_json::Value = if config_path.exists() {
        let content = std::fs::read_to_string(&config_path)?;
        serde_json::from_str(&content)?
    } else {
        serde_json::json!({ "$schema": "https://opencode.ai/config.json" })
    };

    let obj = config
        .as_object_mut()
        .ok_or("opencode.jsonc root is not an object")?;

    let plugin_entry = obj.entry("plugin").or_insert_with(|| serde_json::json!([]));
    let plugin_array = plugin_entry
        .as_array_mut()
        .ok_or("\"plugin\" in opencode.jsonc is not an array")?;
    let dir_str = plugins_dir.to_string_lossy().to_string();
    let plugin_changed = !plugin_array
        .iter()
        .any(|v| v.as_str() == Some(dir_str.as_str()));
    if plugin_changed {
        plugin_array.push(serde_json::Value::String(dir_str));
    }

    let mcp_entry = obj.entry("mcp").or_insert_with(|| serde_json::json!({}));
    let mcp_obj = mcp_entry
        .as_object_mut()
        .ok_or("\"mcp\" in opencode.jsonc is not an object")?;
    let mcp_changed = !mcp_obj.contains_key("suvadu");
    if mcp_changed {
        mcp_obj.insert(
            "suvadu".to_string(),
            serde_json::json!({
                "type": "local",
                "command": [bin_path, "mcp-serve"]
            }),
        );
    }

    if plugin_changed || mcp_changed {
        let updated = serde_json::to_string_pretty(&config)?;
        atomic_write(&config_path, &updated)?;
    }
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

    // Write the afterShellExecution hook script
    let hook_script_path = hooks_dir.join("cursor-after-shell.sh");
    let script = agent_hook::script(&bin_path, "hook-cursor", "cursor");
    crate::util::atomic_write_with_mode(&hook_script_path, &script, 0o700)?;

    // Write the beforeSubmitPrompt hook script
    let prompt_hook_path = hooks_dir.join("cursor-prompt.sh");
    let prompt_script = agent_hook::script(&bin_path, "hook-cursor-prompt", "cursor");
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
            "/usr/local/bin/session-hook.sh",
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
        let session_hook_path = "/home/user/.config/suvadu/hooks/claude-code-session.sh";

        // Start with empty settings
        std::fs::write(&settings_path, "{}").unwrap();

        // First merge should add all hooks
        let result = try_merge_claude_settings(
            &settings_path,
            hook_path,
            failure_hook_path,
            prompt_hook_path,
            session_hook_path,
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
            session_hook_path,
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
        let session_hook_path = "/home/user/.config/suvadu/hooks/claude-code-session.sh";

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
            session_hook_path,
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
            "/path/to/session-hook.sh",
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
        let session_hook_path = "/home/user/.config/suvadu/hooks/claude-code-session.sh";

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
            session_hook_path,
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
                    {"hooks": [
                        {"type": "command", "command": "/path/to/suvadu/hook.sh"},
                        {"type": "command", "command": "/team/mixed-group-hook.sh"}
                    ]}
                ]
            }
        });

        let changed = dedup_suvadu_hooks(&mut settings);
        assert!(changed);

        let arr = settings["hooks"]["UserPromptSubmit"].as_array().unwrap();
        assert_eq!(arr.len(), 3); // 1 suvadu + 2 unrelated handlers/groups
        assert!(arr[0]["hooks"][0]["command"]
            .as_str()
            .unwrap()
            .contains("suvadu"));
        assert!(arr[1]["hooks"][0]["command"]
            .as_str()
            .unwrap()
            .contains("other"));
        assert_eq!(arr[2]["hooks"][0]["command"], "/team/mixed-group-hook.sh");
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

    #[test]
    fn opencode_plugin_keeps_bash_forwarding_and_adds_idle_triggered_import() {
        assert!(OPENCODE_PLUGIN_SCRIPT.contains("tool.execute.after"));
        assert!(OPENCODE_PLUGIN_SCRIPT.contains(r#""bash""#));
        assert!(OPENCODE_PLUGIN_SCRIPT.contains("session.idle"));
        assert!(OPENCODE_PLUGIN_SCRIPT.contains("hook-opencode-session"));
        assert!(OPENCODE_PLUGIN_SCRIPT.contains("client.session.messages"));
        assert!(OPENCODE_PLUGIN_SCRIPT.contains("dispose"));
        // The prompt-caching path is kept (it feeds entries.context["agent_prompt"],
        // a separate mechanism from the session-history import above).
        assert!(OPENCODE_PLUGIN_SCRIPT.contains("cachePrompt"));
        assert!(OPENCODE_PLUGIN_SCRIPT.contains("message.part.updated"));
        // stderr must not be swallowed, or a handler failure is invisible on both sides.
        assert!(OPENCODE_PLUGIN_SCRIPT.contains(r#"stdio: ["pipe", "ignore", "inherit"]"#));
    }

    #[test]
    fn opencode_plugin_caches_prompts_through_suv_instead_of_writing_the_file_itself() {
        // Redaction and the configured max-length live in Rust config, which the
        // plugin can't see — it must hand the raw prompt to `suv` rather than
        // writing (and truncating) the cache file directly, or secrets in
        // prompts over the hardcoded old 500-char cap would never be redacted
        // and would leak into entries.context["agent_prompt"].
        assert!(OPENCODE_PLUGIN_SCRIPT.contains("hook-opencode-prompt"));
        assert!(!OPENCODE_PLUGIN_SCRIPT.contains("writeFileSync"));
        assert!(!OPENCODE_PLUGIN_SCRIPT.contains("mkdirSync"));
        assert!(!OPENCODE_PLUGIN_SCRIPT.contains(".slice(0, 500)"));
    }

    #[test]
    fn opencode_plugin_passes_the_session_directory_to_the_prompt_cache() {
        // Without the directory, the prompt cache falls back to global config
        // while session import (hook-opencode-session, already passed
        // input.directory) resolves a project .suvadu.toml — a mismatch that
        // can leave a secret unredacted, or a length mismatch that breaks
        // reconcile_agent_command_turns' exact-text match, for that project.
        assert!(
            OPENCODE_PLUGIN_SCRIPT.contains("function cachePrompt(sessionID, prompt, directory)")
        );
        assert!(OPENCODE_PLUGIN_SCRIPT.contains(
            r#"["hook-opencode-prompt", "--session-id", sessionID, "--directory", directory]"#
        ));
        assert!(
            OPENCODE_PLUGIN_SCRIPT.contains("cachePrompt(sessionID, part.text, input.directory)")
        );
    }

    #[test]
    fn opencode_plugin_exports_the_bare_factory_shape_the_published_sdk_requires() {
        // The plain `export default async (input) => Hooks` factory function
        // is the documented, correct shape for the actual current stable CLI
        // (confirmed empirically end-to-end against a real local `opencode`
        // v1.18.30 install, matching the published @opencode-ai/plugin@1.18.30
        // npm types exactly: module load, `tool.execute.after` firing on a
        // real bash call, `suv add` succeeding, `session.idle` firing, and
        // `hook-opencode-session` importing the session's messages).
        //
        // Earlier in development this was mistakenly changed to an
        // `{ id, effect: async (input) => Hooks }` object shape, based on
        // empirical testing against a newer, mismatched pre-release CLI build
        // (self-reported version v2.0.1/v2.0.3) that had drifted ahead of the
        // published SDK and used an undocumented alternate contract (where,
        // confusingly, `{ id, setup }` also loads without a shape error but
        // silently never invokes any of its returned hooks — the hooks-return
        // entry point on that build is `effect`, not `setup`). That contract
        // is specific to that mismatched build and must not be reintroduced
        // here; the plain factory function is correct for the CLI version
        // suvadu actually targets.
        assert!(OPENCODE_PLUGIN_SCRIPT.contains("export default async (input) =>"));
        assert!(!OPENCODE_PLUGIN_SCRIPT.contains("id: \"suvadu\""));
        assert!(!OPENCODE_PLUGIN_SCRIPT.contains("effect: async (input) =>"));
        assert!(!OPENCODE_PLUGIN_SCRIPT.contains("setup: async (input) =>"));
    }
}
