use std::fmt::Write;

use serde_json::{json, Value};

use crate::models::SearchField;
use crate::repository::{QueryFilter, Repository};
use crate::util;

/// Maximum number of replay entries to fetch when grouping prompts.
const MAX_PROMPT_ENTRIES: usize = 5000;

/// Return the `tools/list` response, filtering out disabled tools.
pub fn list_tools(id: &Value, mcp: &crate::config::McpConfig) -> Value {
    let all_tools = vec![
        search_commands_def(),
        recent_commands_def(),
        command_status_def(),
        get_prompts_def(),
        session_history_def(),
        get_stats_def(),
        list_sessions_def(),
        what_changed_def(),
        what_failed_def(),
        suggest_next_def(),
        assess_risk_def(),
        find_agent_session_def(),
        replay_agent_session_def(),
        learn_from_failures_def(),
        project_context_def(),
    ];
    let tools: Vec<Value> = all_tools
        .into_iter()
        .filter(|t| {
            let name = t["name"].as_str().unwrap_or("");
            !mcp.disabled_tools.iter().any(|d| d == name)
        })
        .collect();
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "result": { "tools": tools }
    })
}

/// Dispatch a `tools/call` request to the appropriate handler.
pub fn call_tool(
    repo: &Repository,
    name: &str,
    args: &Value,
    mcp: &crate::config::McpConfig,
) -> Result<String, String> {
    if mcp.disabled_tools.iter().any(|d| d == name) {
        return Err(format!("Tool '{name}' is disabled via MCP configuration"));
    }
    match name {
        "search_commands" => handle_search_commands(repo, args),
        "recent_commands" => handle_recent_commands(repo, args),
        "command_status" => handle_command_status(repo, args),
        "get_prompts" => handle_get_prompts(repo, args),
        "session_history" => handle_session_history(repo, args),
        "get_stats" => handle_get_stats(repo, args),
        "list_sessions" => handle_list_sessions(repo, args),
        "what_changed" => handle_what_changed(repo, args),
        "what_failed" => handle_what_failed(repo, args),
        "suggest_next" => handle_suggest_next(repo, args),
        "assess_risk" => handle_assess_risk(args),
        "find_agent_session" => handle_find_agent_session(repo, args),
        "replay_agent_session" => handle_replay_agent_session(repo, args),
        "learn_from_failures" => handle_learn_from_failures(repo, args),
        "project_context" => handle_project_context(repo, args),
        _ => Err(format!("Unknown tool: {name}")),
    }
}

// ── Tool definitions ────────────────────────────────────────

fn search_commands_def() -> Value {
    json!({
        "name": "search_commands",
        "description": "Search shell command history by text pattern. Returns matching commands with directory, exit code, duration, and timestamp.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "query": { "type": "string", "description": "Text to search for in commands" },
                "directory": { "type": "string", "description": "Filter to commands run in this directory" },
                "executor": { "type": "string", "description": "Filter by executor (e.g. claude-code, cursor, human)" },
                "exit_code": { "type": "integer", "description": "Filter by exit code (0 = success)" },
                "after": { "type": "string", "description": "Start date (e.g. today, yesterday, 7 days ago, 2026-01-01)" },
                "before": { "type": "string", "description": "End date (e.g. today, yesterday, 7 days ago, 2026-01-01)" },
                "limit": { "type": "integer", "description": "Max results to return (default: 20)", "default": 20 }
            },
            "required": ["query"]
        }
    })
}

fn recent_commands_def() -> Value {
    json!({
        "name": "recent_commands",
        "description": "Get the most recent commands, optionally filtered by directory. Use this to understand what happened recently in a project.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "directory": { "type": "string", "description": "Filter to commands run in this directory" },
                "executor": { "type": "string", "description": "Filter by executor (e.g. claude-code, cursor)" },
                "after": { "type": "string", "description": "Start date (e.g. today, yesterday, 7 days ago, 2026-01-01)" },
                "limit": { "type": "integer", "description": "Max results (default: 20)", "default": 20 }
            }
        }
    })
}

fn command_status_def() -> Value {
    json!({
        "name": "command_status",
        "description": "Check if a specific command has been run before and what happened. Returns previous runs with exit codes and timestamps. Useful to check if a command typically succeeds or fails.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "command": { "type": "string", "description": "Command text to search for (prefix match)" },
                "directory": { "type": "string", "description": "Filter to this directory" },
                "limit": { "type": "integer", "description": "Max previous runs (default: 5)", "default": 5 }
            },
            "required": ["command"]
        }
    })
}

fn get_prompts_def() -> Value {
    json!({
        "name": "get_prompts",
        "description": "Browse AI agent prompts and the commands they triggered. Shows what prompt led to which commands, with exit codes. Useful to understand what a previous agent session did.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "executor": { "type": "string", "description": "Filter by executor (e.g. claude-code, cursor)" },
                "session_id": { "type": "string", "description": "Filter to a specific session" },
                "after": { "type": "string", "description": "Start date" },
                "limit": { "type": "integer", "description": "Max prompts (default: 10)", "default": 10 }
            }
        }
    })
}

fn session_history_def() -> Value {
    json!({
        "name": "session_history",
        "description": "Get the full command history of a specific session in chronological order.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "session_id": { "type": "string", "description": "Session ID (defaults to most recent)" },
                "limit": { "type": "integer", "description": "Max commands (default: 50)", "default": 50 }
            }
        }
    })
}

fn get_stats_def() -> Value {
    json!({
        "name": "get_stats",
        "description": "Get aggregate statistics about shell history: total commands, success rate, top commands, and top directories.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "days": { "type": "integer", "description": "Time window in days (default: 7)", "default": 7 },
                "directory": { "type": "string", "description": "Filter to this directory" }
            }
        }
    })
}

fn list_sessions_def() -> Value {
    json!({
        "name": "list_sessions",
        "description": "List recent shell sessions with command counts, time ranges, and tags.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "limit": { "type": "integer", "description": "Max sessions (default: 10)", "default": 10 },
                "tag": { "type": "string", "description": "Filter by tag name" }
            }
        }
    })
}

fn what_changed_def() -> Value {
    json!({
        "name": "what_changed",
        "description": "Analyze what file-modifying operations happened in a directory recently. Classifies commands into categories: file writes, deletions, git operations, package installs, config changes. Use this to understand what an agent or user changed before you start working.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "directory": { "type": "string", "description": "Directory to analyze (defaults to all)" },
                "hours": { "type": "integer", "description": "How many hours back to look (default: 4)", "default": 4 },
                "executor": { "type": "string", "description": "Filter by executor (e.g. claude-code, cursor)" }
            }
        }
    })
}

fn what_failed_def() -> Value {
    json!({
        "name": "what_failed",
        "description": "Show recent command failures with the prompts that caused them. Groups failures by the AI prompt or session that triggered them, so you can understand what went wrong and avoid repeating the same mistakes.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "directory": { "type": "string", "description": "Filter to this directory" },
                "hours": { "type": "integer", "description": "How many hours back to look (default: 24)", "default": 24 },
                "limit": { "type": "integer", "description": "Max failures to show (default: 20)", "default": 20 }
            }
        }
    })
}

fn suggest_next_def() -> Value {
    json!({
        "name": "suggest_next",
        "description": "Predict what commands are likely to be run next based on recent history and current directory. Uses frecency (frequency + recency) to rank suggestions. Useful for understanding the typical workflow in a project.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "directory": { "type": "string", "description": "Directory context (defaults to all)" },
                "limit": { "type": "integer", "description": "Number of suggestions (default: 10)", "default": 10 }
            }
        }
    })
}

fn assess_risk_def() -> Value {
    json!({
        "name": "assess_risk",
        "description": "Assess the risk level of a command BEFORE running it. Returns safe/low/medium/high/critical with category and explanation. Use this to check if a command is destructive before executing it.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "command": { "type": "string", "description": "The command to assess (e.g. 'rm -rf /tmp', 'git push --force')" },
                "commands": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "Multiple commands to assess at once"
                }
            }
        }
    })
}

fn find_agent_session_def() -> Value {
    json!({
        "name": "find_agent_session",
        "description": "Search past AI agent sessions. Find sessions by prompt text, directory, executor, or date range. Returns session summaries with command counts, success rates, and the first prompt. Use this to discover what previous agent sessions did in a project.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "directory": { "type": "string", "description": "Filter to sessions that ran commands in this directory" },
                "executor": { "type": "string", "description": "Filter by agent name (e.g. claude-code, cursor)" },
                "prompt_text": { "type": "string", "description": "Search across prompts in sessions (substring match)" },
                "after": { "type": "string", "description": "Only sessions after this date (ISO 8601 or relative like '3 days ago')" },
                "before": { "type": "string", "description": "Only sessions before this date" },
                "limit": { "type": "integer", "description": "Max sessions to return (default: 10)", "default": 10 }
            }
        }
    })
}

fn replay_agent_session_def() -> Value {
    json!({
        "name": "replay_agent_session",
        "description": "Get the full chronological timeline of a specific agent session: every prompt and command with exit codes, directories, and timestamps. Use this to understand exactly what a past agent session did.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "session_id": { "type": "string", "description": "The session ID to replay (with or without claude-/cursor- prefix)" },
                "limit": { "type": "integer", "description": "Max commands to return (default: 100)", "default": 100 }
            },
            "required": ["session_id"]
        }
    })
}

fn learn_from_failures_def() -> Value {
    json!({
        "name": "learn_from_failures",
        "description": "Analyze recurring command failures in a project. Shows commands with high failure rates, whether agents fail more than humans, and recent failure-to-fix patterns. Use this before starting work to avoid repeating known-bad approaches.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "directory": { "type": "string", "description": "Directory to analyze (defaults to all)" },
                "days": { "type": "integer", "description": "How many days back to look (default: 7)", "default": 7 }
            }
        }
    })
}

fn project_context_def() -> Value {
    json!({
        "name": "project_context",
        "description": "Get a project briefing: common commands, build/test/lint patterns, recent failures, failure rates, and agent activity. Use this to understand a project's workflow before making changes.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "directory": { "type": "string", "description": "Directory to analyze (defaults to all)" },
                "days": { "type": "integer", "description": "Time window in days (default: 7)", "default": 7 }
            }
        }
    })
}

// ── Tool handlers ───────────────────────────────────────────

fn get_int(args: &Value, key: &str, default: i64) -> i64 {
    args.get(key).and_then(Value::as_i64).unwrap_or(default)
}

fn get_str<'a>(args: &'a Value, key: &str) -> Option<&'a str> {
    args.get(key)
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
}

fn format_entry(e: &crate::models::Entry) -> String {
    let exit = match e.exit_code {
        Some(0) => "ok".to_string(),
        Some(c) => format!("exit {c}"),
        None => "unknown".to_string(),
    };
    let time = format_time(e.started_at);
    let dur = util::format_duration_ms(e.duration_ms);
    let executor = match (&e.executor_type, &e.executor) {
        (Some(et), Some(n)) => format!("{et}: {n}"),
        (Some(et), None) => et.clone(),
        (None, Some(n)) => n.clone(),
        _ => "unknown".to_string(),
    };
    format!(
        "  {} | {} | {} | {} | {}\n    dir: {}",
        e.command, exit, dur, time, executor, e.cwd,
    )
}

fn format_time(ms: i64) -> String {
    let ms_val = util::normalize_display_ms(ms);
    chrono::Local
        .timestamp_millis_opt(ms_val)
        .single()
        .map_or_else(
            || "unknown".to_string(),
            |dt| dt.format("%Y-%m-%d %H:%M:%S").to_string(),
        )
}

use chrono::TimeZone;

fn handle_search_commands(repo: &Repository, args: &Value) -> Result<String, String> {
    let query = get_str(args, "query").unwrap_or("");
    let limit = usize::try_from(get_int(args, "limit", 20)).unwrap_or(20);
    let after = get_str(args, "after").and_then(|s| util::parse_date_input(s, false));
    let before = get_str(args, "before").and_then(|s| util::parse_date_input(s, true));
    let executor = get_str(args, "executor");
    let directory = get_str(args, "directory");
    let exit_code = args
        .get("exit_code")
        .and_then(Value::as_i64)
        .map(|c| i32::try_from(c).unwrap_or(0));

    let qf = QueryFilter {
        after,
        before,
        tag_id: None,
        exit_code,
        query: if query.is_empty() { None } else { Some(query) },
        prefix_match: false,
        executor,
        cwd: directory,
        field: SearchField::Command,
        exclude_agents: false,
    };

    let entries = repo
        .get_entries_filtered(limit, 0, &qf)
        .map_err(|e| format!("query failed: {e}"))?;

    if entries.is_empty() {
        return Ok(format!("No commands found matching \"{query}\"."));
    }

    let mut out = format!(
        "Found {} commands matching \"{}\":\n\n",
        entries.len(),
        query
    );
    for (i, e) in entries.iter().enumerate() {
        let _ = writeln!(out, "{}. {}", i + 1, format_entry(e));
        out.push('\n');
    }
    Ok(out)
}

fn handle_recent_commands(repo: &Repository, args: &Value) -> Result<String, String> {
    let limit = usize::try_from(get_int(args, "limit", 20)).unwrap_or(20);
    let directory = get_str(args, "directory");
    let executor = get_str(args, "executor");
    let after = get_str(args, "after").and_then(|s| util::parse_date_input(s, false));

    let entries = if executor.is_some() || after.is_some() {
        // Use filtered query when executor or after is specified
        let qf = QueryFilter {
            after,
            before: None,
            tag_id: None,
            exit_code: None,
            query: None,
            prefix_match: false,
            executor,
            cwd: directory,
            field: SearchField::Command,
            exclude_agents: false,
        };
        repo.get_entries_filtered(limit, 0, &qf)
            .map_err(|e| format!("query failed: {e}"))?
    } else {
        repo.get_recent_entries(limit, 0, None, false, directory, true)
            .map_err(|e| format!("query failed: {e}"))?
    };

    if entries.is_empty() {
        let ctx = directory.map_or_else(String::new, |d| format!(" in {d}"));
        return Ok(format!("No recent commands found{ctx}."));
    }

    let ctx = directory.map_or_else(String::new, |d| format!(" in {d}"));
    let mut out = format!("{} most recent commands{ctx}:\n\n", entries.len());
    for (i, e) in entries.iter().enumerate() {
        let _ = writeln!(out, "{}. {}", i + 1, format_entry(e));
        out.push('\n');
    }
    Ok(out)
}

fn handle_command_status(repo: &Repository, args: &Value) -> Result<String, String> {
    let command = get_str(args, "command").unwrap_or("");
    let limit = usize::try_from(get_int(args, "limit", 5)).unwrap_or(5);
    let directory = get_str(args, "directory");

    if command.is_empty() {
        return Err("command parameter is required".to_string());
    }

    let entries = repo
        .get_recent_entries(limit, 0, Some(command), true, directory, true)
        .map_err(|e| format!("query failed: {e}"))?;

    if entries.is_empty() {
        return Ok(format!("No previous runs of \"{command}\" found."));
    }

    let total = entries.len();
    let successes = entries.iter().filter(|e| e.exit_code == Some(0)).count();
    let failures = entries
        .iter()
        .filter(|e| e.exit_code.is_some_and(|c| c != 0))
        .count();

    let mut out = format!(
        "\"{command}\" — {total} recent runs ({successes} succeeded, {failures} failed):\n\n",
    );
    for (i, e) in entries.iter().enumerate() {
        let _ = writeln!(out, "{}. {}", i + 1, format_entry(e));
        out.push('\n');
    }
    Ok(out)
}

fn handle_get_prompts(repo: &Repository, args: &Value) -> Result<String, String> {
    let limit = usize::try_from(get_int(args, "limit", 10)).unwrap_or(10);
    let after = get_str(args, "after").and_then(|s| util::parse_date_input(s, false));
    let executor = get_str(args, "executor");
    let session_filter = get_str(args, "session_id");

    // Load agent entries
    let entries = repo
        .get_replay_entries(
            session_filter,
            &crate::repository::ReplayFilter {
                after,
                executor,
                limit: Some(MAX_PROMPT_ENTRIES),
                ..Default::default()
            },
        )
        .map_err(|e| format!("query failed: {e}"))?;

    // Group by (session_id, prompt)
    let mut groups: std::collections::HashMap<(String, String), Vec<&crate::models::Entry>> =
        std::collections::HashMap::new();

    for entry in &entries {
        let prompt = entry
            .context
            .as_ref()
            .and_then(|ctx| ctx.get("agent_prompt"))
            .cloned()
            .unwrap_or_default();
        if prompt.is_empty() {
            continue;
        }
        groups
            .entry((entry.session_id.clone(), prompt))
            .or_default()
            .push(entry);
    }

    if groups.is_empty() {
        return Ok("No agent prompts found.".to_string());
    }

    // Sort by most recent command timestamp, take `limit`
    let mut sorted: Vec<_> = groups.into_iter().collect();
    sorted.sort_by(|a, b| {
        let a_max = a.1.iter().map(|e| e.started_at).max().unwrap_or(0);
        let b_max = b.1.iter().map(|e| e.started_at).max().unwrap_or(0);
        b_max.cmp(&a_max)
    });
    sorted.truncate(limit);

    let mut out = format!("{} prompts found:\n\n", sorted.len());
    for (i, ((session_id, prompt), cmds)) in sorted.iter().enumerate() {
        let successes = cmds.iter().filter(|e| e.exit_code == Some(0)).count();
        let session_short: String = session_id
            .strip_prefix("claude-")
            .or_else(|| session_id.strip_prefix("cursor-"))
            .or_else(|| session_id.strip_prefix("opencode-"))
            .unwrap_or(session_id)
            .chars()
            .take(12)
            .collect();
        let _ = writeln!(
            out,
            "{}. [{}] \"{}\" — {} cmds, {} ok",
            i + 1,
            session_short,
            prompt,
            cmds.len(),
            successes,
        );
        for cmd in cmds.iter().take(5) {
            let exit = match cmd.exit_code {
                Some(0) => "ok",
                Some(_) => "FAIL",
                None => "?",
            };
            let _ = writeln!(out, "     {} [{}]", cmd.command, exit);
        }
        if cmds.len() > 5 {
            let _ = writeln!(out, "     ... and {} more", cmds.len() - 5);
        }
        out.push('\n');
    }
    Ok(out)
}

fn handle_session_history(repo: &Repository, args: &Value) -> Result<String, String> {
    let session_id = get_str(args, "session_id");
    let limit = usize::try_from(get_int(args, "limit", 50)).unwrap_or(50);

    let entries = repo
        .get_replay_entries(
            session_id,
            &crate::repository::ReplayFilter {
                limit: Some(limit),
                ..Default::default()
            },
        )
        .map_err(|e| format!("query failed: {e}"))?;

    if entries.is_empty() {
        return Ok(session_id.map_or_else(
            || "No commands found in any session.".to_string(),
            |id| format!("No commands found in session {id}."),
        ));
    }

    let sid = entries.first().map_or("unknown", |e| e.session_id.as_str());
    let mut out = format!("Session {} — {} commands:\n\n", sid, entries.len());
    for (i, e) in entries.iter().enumerate() {
        let _ = writeln!(out, "{}. {}", i + 1, format_entry(e));
        out.push('\n');
    }
    Ok(out)
}

fn handle_get_stats(repo: &Repository, args: &Value) -> Result<String, String> {
    let days = get_int(args, "days", 7);
    let directory = get_str(args, "directory");

    let now = chrono::Utc::now().timestamp_millis();
    let after = Some(now - days * 24 * 60 * 60 * 1000);

    let qf = QueryFilter {
        after,
        before: None,
        tag_id: None,
        exit_code: None,
        query: None,
        prefix_match: false,
        executor: None,
        cwd: directory,
        field: SearchField::Command,
        exclude_agents: false,
    };

    let total = repo
        .count_filtered(&qf)
        .map_err(|e| format!("query failed: {e}"))?;

    let success_qf = QueryFilter {
        exit_code: Some(0),
        ..qf
    };
    let successes = repo
        .count_filtered(&success_qf)
        .map_err(|e| format!("query failed: {e}"))?;

    let rate = if total > 0 {
        successes * 100 / total
    } else {
        0
    };

    // Get top commands
    let entries = repo
        .get_entries_filtered(200, 0, &qf)
        .map_err(|e| format!("query failed: {e}"))?;

    let mut cmd_counts: std::collections::HashMap<&str, i64> = std::collections::HashMap::new();
    let mut dir_counts: std::collections::HashMap<&str, i64> = std::collections::HashMap::new();
    for e in &entries {
        let program = e.command.split_whitespace().next().unwrap_or(&e.command);
        *cmd_counts.entry(program).or_default() += 1;
        *dir_counts.entry(e.cwd.as_str()).or_default() += 1;
    }

    let mut top_cmds: Vec<_> = cmd_counts.into_iter().collect();
    top_cmds.sort_by_key(|b| std::cmp::Reverse(b.1));
    top_cmds.truncate(10);

    let mut top_dirs: Vec<_> = dir_counts.into_iter().collect();
    top_dirs.sort_by_key(|b| std::cmp::Reverse(b.1));
    top_dirs.truncate(5);

    let dir_ctx = directory.map_or_else(String::new, |d| format!(" in {d}"));
    let mut out = format!(
        "Stats for the last {days} days{dir_ctx}:\n\n  Total commands: {total}\n  Success rate: {rate}%\n\n",
    );

    out.push_str("Top commands:\n");
    for (cmd, count) in &top_cmds {
        let _ = writeln!(out, "  {count:>4}x  {cmd}");
    }

    out.push_str("\nTop directories:\n");
    for (dir, count) in &top_dirs {
        let _ = writeln!(out, "  {count:>4}x  {dir}");
    }

    Ok(out)
}

fn handle_list_sessions(repo: &Repository, args: &Value) -> Result<String, String> {
    let limit = usize::try_from(get_int(args, "limit", 10)).unwrap_or(10);
    let tag = get_str(args, "tag");

    let tag_id = tag.and_then(|t| repo.get_tag_id_by_name(t).ok().flatten());

    let sessions = repo
        .list_sessions(None, tag_id, limit)
        .map_err(|e| format!("query failed: {e}"))?;

    if sessions.is_empty() {
        return Ok("No sessions found.".to_string());
    }

    let mut out = format!("{} sessions:\n\n", sessions.len());
    for (i, s) in sessions.iter().enumerate() {
        let tag_str = s
            .tag_name
            .as_deref()
            .map_or_else(String::new, |t| format!(" [{t}]"));
        let first = format_time(s.first_cmd_at);
        let last = format_time(s.last_cmd_at);
        let _ = write!(
            out,
            "{}. {}{}\n   {} cmds | {} ok | {} — {}\n\n",
            i + 1,
            s.id,
            tag_str,
            s.cmd_count,
            s.success_count,
            first,
            last,
        );
    }
    Ok(out)
}

// ── Smart tools ─────────────────────────────────────────────

/// Classify a command into a change category for `what_changed`.
fn classify_command(cmd: &str) -> Option<&'static str> {
    let cmd = cmd.trim();
    let first = cmd.split_whitespace().next().unwrap_or("");

    // File deletions
    if first == "rm" || first == "rmdir" || cmd.starts_with("rm ") {
        return Some("deletions");
    }
    // File moves/renames
    if first == "mv" {
        return Some("moves/renames");
    }
    // File copies
    if first == "cp" {
        return Some("copies");
    }
    // File creation/writes
    if first == "touch"
        || first == "mkdir"
        || first == "tee"
        || cmd.contains(" > ")
        || cmd.contains(" >> ")
    {
        return Some("file writes");
    }
    // Editors
    if matches!(first, "vim" | "nvim" | "nano" | "vi" | "code" | "sed") {
        return Some("file edits");
    }
    // Git operations
    if first == "git" {
        let sub = cmd.split_whitespace().nth(1).unwrap_or("");
        return match sub {
            "commit" | "merge" | "rebase" | "cherry-pick" | "revert" => Some("git commits"),
            "push" | "pull" | "fetch" => Some("git sync"),
            "checkout" | "switch" | "branch" => Some("git branches"),
            "add" | "rm" | "reset" | "restore" | "stash" => Some("git staging"),
            _ => None,
        };
    }
    // Package installs
    if matches!(
        first,
        "npm" | "yarn" | "pnpm" | "pip" | "pip3" | "cargo" | "brew" | "apt"
    ) && cmd.contains("install")
    {
        return Some("package installs");
    }
    // Docker
    if first == "docker" || first == "docker-compose" {
        return Some("docker");
    }
    // Build commands
    if matches!(first, "make" | "cmake" | "cargo") {
        let sub = cmd.split_whitespace().nth(1).unwrap_or("");
        if matches!(sub, "build" | "compile" | "release") {
            return Some("builds");
        }
    }
    // chmod/chown
    if matches!(first, "chmod" | "chown") {
        return Some("permission changes");
    }
    None
}

fn handle_what_changed(repo: &Repository, args: &Value) -> Result<String, String> {
    let hours = get_int(args, "hours", 4);
    let directory = get_str(args, "directory");
    let executor = get_str(args, "executor");

    let now = chrono::Utc::now().timestamp_millis();
    let after = Some(now - hours * 60 * 60 * 1000);

    let qf = QueryFilter {
        after,
        before: None,
        tag_id: None,
        exit_code: None,
        query: None,
        prefix_match: false,
        executor,
        cwd: directory,
        field: SearchField::Command,
        exclude_agents: false,
    };

    let entries = repo
        .get_entries_filtered(500, 0, &qf)
        .map_err(|e| format!("query failed: {e}"))?;

    // Classify commands into categories
    let mut categories: std::collections::HashMap<&str, Vec<&crate::models::Entry>> =
        std::collections::HashMap::new();
    let mut unclassified = 0usize;

    for entry in &entries {
        if let Some(category) = classify_command(&entry.command) {
            categories.entry(category).or_default().push(entry);
        } else {
            unclassified += 1;
        }
    }

    if categories.is_empty() {
        let ctx = directory.map_or_else(String::new, |d| format!(" in {d}"));
        return Ok(format!(
            "No file-modifying operations found in the last {hours} hours{ctx}. ({} total commands, all read-only.)",
            entries.len()
        ));
    }

    let ctx = directory.map_or_else(String::new, |d| format!(" in {d}"));
    let mut out = String::new();
    let _ = writeln!(
        out,
        "Changes in the last {hours} hours{ctx} ({} total commands):\n",
        entries.len()
    );

    // Sort categories by count descending
    let mut sorted: Vec<_> = categories.into_iter().collect();
    sorted.sort_by_key(|b| std::cmp::Reverse(b.1.len()));

    for (category, cmds) in &sorted {
        let _ = writeln!(out, "  {} ({}):", category.to_uppercase(), cmds.len());
        for cmd in cmds.iter().take(5) {
            let exit = match cmd.exit_code {
                Some(0) => "ok",
                Some(_) => "FAIL",
                None => "?",
            };
            let _ = writeln!(out, "    {} [{}]", cmd.command, exit);
        }
        if cmds.len() > 5 {
            let _ = writeln!(out, "    ... and {} more", cmds.len() - 5);
        }
        out.push('\n');
    }

    if unclassified > 0 {
        let _ = writeln!(
            out,
            "  ({unclassified} other commands not shown — read-only or unclassified)"
        );
    }

    Ok(out)
}

fn handle_what_failed(repo: &Repository, args: &Value) -> Result<String, String> {
    let hours = get_int(args, "hours", 24);
    let limit = usize::try_from(get_int(args, "limit", 20)).unwrap_or(20);
    let directory = get_str(args, "directory");

    let now = chrono::Utc::now().timestamp_millis();
    let after = Some(now - hours * 60 * 60 * 1000);

    // Get all entries in the time window, then filter to failures
    let qf = QueryFilter {
        after,
        before: None,
        tag_id: None,
        exit_code: None,
        query: None,
        prefix_match: false,
        executor: None,
        cwd: directory,
        field: SearchField::Command,
        exclude_agents: false,
    };

    let entries = repo
        .get_entries_filtered(1000, 0, &qf)
        .map_err(|e| format!("query failed: {e}"))?;

    let failures: Vec<_> = entries
        .iter()
        .filter(|e| e.exit_code.is_some_and(|c| c != 0))
        .take(limit)
        .collect();

    if failures.is_empty() {
        let ctx = directory.map_or_else(String::new, |d| format!(" in {d}"));
        return Ok(format!(
            "No failures in the last {hours} hours{ctx}. {} commands all succeeded.",
            entries.len()
        ));
    }

    // Group failures by prompt (if available)
    let mut by_prompt: std::collections::HashMap<String, Vec<&&crate::models::Entry>> =
        std::collections::HashMap::new();
    let mut no_prompt_failures: Vec<&&crate::models::Entry> = Vec::new();

    for entry in &failures {
        let prompt = entry
            .context
            .as_ref()
            .and_then(|ctx| ctx.get("agent_prompt"))
            .cloned();
        if let Some(p) = prompt {
            by_prompt.entry(p).or_default().push(entry);
        } else {
            no_prompt_failures.push(entry);
        }
    }

    let ctx = directory.map_or_else(String::new, |d| format!(" in {d}"));
    let mut out = String::new();
    let _ = writeln!(
        out,
        "{} failures in the last {hours} hours{ctx}:\n",
        failures.len()
    );

    // Show prompt-grouped failures first
    if !by_prompt.is_empty() {
        let _ = writeln!(out, "FAILURES TRIGGERED BY PROMPTS:");
        let mut sorted: Vec<_> = by_prompt.into_iter().collect();
        sorted.sort_by_key(|b| std::cmp::Reverse(b.1.len()));

        for (prompt, cmds) in &sorted {
            let _ = writeln!(out, "\n  Prompt: \"{prompt}\"");
            let _ = writeln!(out, "  {} commands failed:", cmds.len());
            for cmd in cmds.iter().take(5) {
                let code = cmd.exit_code.unwrap_or(-1);
                let _ = writeln!(
                    out,
                    "    exit {} | {} | {}",
                    code,
                    cmd.command,
                    format_time(cmd.started_at)
                );
            }
            if cmds.len() > 5 {
                let _ = writeln!(out, "    ... and {} more", cmds.len() - 5);
            }
        }
        out.push('\n');
    }

    // Show non-prompt failures
    if !no_prompt_failures.is_empty() {
        let _ = writeln!(out, "OTHER FAILURES (no prompt captured):");
        for cmd in no_prompt_failures.iter().take(10) {
            let code = cmd.exit_code.unwrap_or(-1);
            let executor = cmd.executor.as_deref().unwrap_or("unknown");
            let _ = writeln!(
                out,
                "  exit {} | {} | {} | {}",
                code,
                cmd.command,
                executor,
                format_time(cmd.started_at)
            );
        }
        if no_prompt_failures.len() > 10 {
            let _ = writeln!(out, "  ... and {} more", no_prompt_failures.len() - 10);
        }
    }

    Ok(out)
}

fn handle_suggest_next(repo: &Repository, args: &Value) -> Result<String, String> {
    let limit = usize::try_from(get_int(args, "limit", 10)).unwrap_or(10);
    let directory = get_str(args, "directory");

    // Get recent commands (last 7 days) to build frecency scores
    let now = chrono::Utc::now().timestamp_millis();
    let week_ago = now - 7 * 24 * 60 * 60 * 1000;

    let qf = QueryFilter {
        after: Some(week_ago),
        before: None,
        tag_id: None,
        exit_code: None,
        query: None,
        prefix_match: false,
        executor: None,
        cwd: directory,
        field: SearchField::Command,
        exclude_agents: false,
    };

    let entries = repo
        .get_entries_filtered(2000, 0, &qf)
        .map_err(|e| format!("query failed: {e}"))?;

    if entries.is_empty() {
        let ctx = directory.map_or_else(String::new, |d| format!(" in {d}"));
        return Ok(format!(
            "No recent commands found{ctx} to base suggestions on."
        ));
    }

    // Score each unique command by frecency
    // Score = sum(weight) where weight depends on recency tier
    let mut scores: std::collections::HashMap<&str, (f64, usize, Option<i32>)> =
        std::collections::HashMap::new(); // cmd -> (score, count, last_exit)

    for entry in &entries {
        #[allow(clippy::cast_precision_loss)]
        let age_hours = (now - entry.started_at) as f64 / 3_600_000.0;
        let weight = if age_hours < 1.0 {
            16.0 // last hour
        } else if age_hours < 24.0 {
            8.0 // today
        } else if age_hours < 72.0 {
            4.0 // last 3 days
        } else {
            1.0 // older
        };

        let (score, count, last_exit) = scores.entry(&entry.command).or_insert((0.0, 0, None));
        *score += weight;
        *count += 1;
        if last_exit.is_none() {
            *last_exit = entry.exit_code;
        }
    }

    // Sort by score descending
    let mut sorted: Vec<_> = scores.into_iter().collect();
    sorted.sort_by(|a, b| {
        b.1 .0
            .partial_cmp(&a.1 .0)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    sorted.truncate(limit);

    let ctx = directory.map_or_else(String::new, |d| format!(" in {d}"));
    let mut out = String::new();
    let _ = writeln!(out, "Suggested next commands{ctx} (based on frecency):\n");
    for (i, (cmd, (score, count, last_exit))) in sorted.iter().enumerate() {
        let exit_display = match last_exit {
            Some(0) => "last: ok".to_string(),
            Some(c) => format!("last: exit {c}"),
            None => "last: ?".to_string(),
        };
        let _ = writeln!(
            out,
            "  {}. {} ({}x, score: {:.0}, {})",
            i + 1,
            cmd,
            count,
            score,
            exit_display,
        );
    }

    Ok(out)
}

fn handle_assess_risk(args: &Value) -> Result<String, String> {
    use crate::risk;

    // Support single command or batch
    let commands: Vec<&str> = if let Some(cmd) = get_str(args, "command") {
        vec![cmd]
    } else if let Some(arr) = args.get("commands").and_then(Value::as_array) {
        arr.iter().filter_map(Value::as_str).collect()
    } else {
        return Err("Provide either 'command' (string) or 'commands' (array)".to_string());
    };

    if commands.is_empty() {
        return Err("No commands provided".to_string());
    }

    let mut out = String::new();

    if commands.len() == 1 {
        let cmd = commands[0];
        let assessment = risk::assess_risk(cmd);
        let level = risk::risk_level(cmd);

        let _ = writeln!(out, "Risk assessment for: {cmd}\n");
        let _ = writeln!(out, "  Level: {}", level.label().to_uppercase());

        if let Some(a) = assessment {
            let _ = writeln!(out, "  Category: {}", a.category);
            let _ = writeln!(out, "  Reason: {}", a.description);
        } else {
            let _ = writeln!(out, "  No known risk patterns detected.");
        }
    } else {
        let _ = writeln!(out, "Risk assessment for {} commands:\n", commands.len());

        let mut critical = 0usize;
        let mut high = 0usize;
        let mut medium = 0usize;

        for cmd in &commands {
            let level = risk::risk_level(cmd);
            let assessment = risk::assess_risk(cmd);
            let label = level.label().to_uppercase();

            match level {
                risk::RiskLevel::Critical => critical += 1,
                risk::RiskLevel::High => high += 1,
                risk::RiskLevel::Medium => medium += 1,
                _ => {}
            }

            if level >= risk::RiskLevel::Medium {
                let reason = assessment.as_ref().map_or("", |a| a.description);
                let _ = writeln!(out, "  {label}: {cmd}");
                if !reason.is_empty() {
                    let _ = writeln!(out, "    → {reason}");
                }
            } else {
                let _ = writeln!(out, "  {label}: {cmd}");
            }
        }

        out.push('\n');
        if critical > 0 || high > 0 {
            let _ = writeln!(
                out,
                "⚠ WARNING: {critical} critical, {high} high-risk commands detected."
            );
        } else if medium > 0 {
            let _ = writeln!(
                out,
                "⚡ {medium} medium-risk commands detected. Review before executing."
            );
        } else {
            let _ = writeln!(out, "All commands appear safe.");
        }
    }

    Ok(out)
}

// ── Agent session tools ─────────────────────────────────────

struct AgentSessionSummary {
    session_id: String,
    executor: String,
    first_prompt: String,
    command_count: usize,
    success_count: usize,
    failure_count: usize,
    directories: Vec<String>,
    first_command_at: i64,
    last_command_at: i64,
    risk_summary: String,
}

/// Group agent entries by `session_id` and compute per-session summaries.
fn build_session_groups(entries: &[crate::models::Entry]) -> Vec<AgentSessionSummary> {
    use crate::risk;
    use std::collections::{HashMap, HashSet};

    let mut groups: HashMap<&str, Vec<&crate::models::Entry>> = HashMap::new();
    for entry in entries {
        if entry.executor_type.as_deref() != Some("agent") {
            continue;
        }
        groups
            .entry(entry.session_id.as_str())
            .or_default()
            .push(entry);
    }
    // Sort each group chronologically so first_prompt is the earliest
    for cmds in groups.values_mut() {
        cmds.sort_by_key(|e| e.started_at);
    }

    let mut sessions: Vec<AgentSessionSummary> = groups
        .into_iter()
        .map(|(session_id, cmds)| {
            let command_count = cmds.len();
            let success_count = cmds.iter().filter(|e| e.exit_code == Some(0)).count();
            let failure_count = cmds
                .iter()
                .filter(|e| e.exit_code.is_some_and(|c| c != 0))
                .count();

            let mut dirs = HashSet::new();
            for e in &cmds {
                dirs.insert(e.cwd.as_str());
            }
            let mut directories: Vec<String> = dirs.into_iter().map(String::from).collect();
            directories.sort();
            directories.truncate(3);

            let first_prompt = cmds
                .iter()
                .find_map(|e| {
                    e.context
                        .as_ref()
                        .and_then(|ctx| ctx.get("agent_prompt"))
                        .filter(|p| !p.is_empty())
                })
                .cloned()
                .unwrap_or_default();

            let executor = cmds
                .first()
                .and_then(|e| e.executor.clone())
                .unwrap_or_else(|| "unknown".to_string());

            let first_command_at = cmds.iter().map(|e| e.started_at).min().unwrap_or(0);
            let last_command_at = cmds.iter().map(|e| e.started_at).max().unwrap_or(0);

            let risk_data = risk::session_risk(&cmds.iter().copied().cloned().collect::<Vec<_>>());
            let mut risk_parts = Vec::new();
            if risk_data.critical_count > 0 {
                risk_parts.push(format!("{} critical", risk_data.critical_count));
            }
            if risk_data.high_count > 0 {
                risk_parts.push(format!("{} high", risk_data.high_count));
            }
            if risk_data.medium_count > 0 {
                risk_parts.push(format!("{} medium", risk_data.medium_count));
            }
            let safe = risk_data.safe_count + risk_data.low_count;
            if safe > 0 {
                risk_parts.push(format!("{safe} safe"));
            }
            let risk_summary = if risk_parts.is_empty() {
                "all safe".to_string()
            } else {
                risk_parts.join(", ")
            };

            AgentSessionSummary {
                session_id: session_id.to_string(),
                executor,
                first_prompt,
                command_count,
                success_count,
                failure_count,
                directories,
                first_command_at,
                last_command_at,
                risk_summary,
            }
        })
        .collect();

    sessions.sort_by_key(|b| std::cmp::Reverse(b.last_command_at));
    sessions
}

/// Extract the resume ID by stripping the agent prefix from a session ID.
fn resume_id(session_id: &str) -> &str {
    session_id
        .strip_prefix("claude-")
        .or_else(|| session_id.strip_prefix("cursor-"))
        .or_else(|| session_id.strip_prefix("opencode-"))
        .unwrap_or(session_id)
}

/// Format a relative time description like "3 hours ago" from millisecond timestamps.
fn format_relative_time(ms: i64) -> String {
    let now = chrono::Utc::now().timestamp_millis();
    let diff = now - ms;
    if diff < 0 {
        return "just now".to_string();
    }
    let minutes = diff / 60_000;
    let hours = minutes / 60;
    let days = hours / 24;
    if days > 0 {
        format!("{days} day{} ago", if days == 1 { "" } else { "s" })
    } else if hours > 0 {
        format!("{hours} hour{} ago", if hours == 1 { "" } else { "s" })
    } else if minutes > 0 {
        format!(
            "{minutes} minute{} ago",
            if minutes == 1 { "" } else { "s" }
        )
    } else {
        "just now".to_string()
    }
}

fn handle_find_agent_session(repo: &Repository, args: &Value) -> Result<String, String> {
    let limit = usize::try_from(get_int(args, "limit", 10)).unwrap_or(10);
    let directory = get_str(args, "directory");
    let executor = get_str(args, "executor");
    let prompt_text = get_str(args, "prompt_text");
    let after = get_str(args, "after").and_then(|s| util::parse_date_input(s, false));
    let before = get_str(args, "before").and_then(|s| util::parse_date_input(s, true));

    let qf = QueryFilter {
        after,
        before,
        tag_id: None,
        exit_code: None,
        query: None,
        prefix_match: false,
        executor,
        cwd: directory,
        field: crate::models::SearchField::Command,
        exclude_agents: false,
    };

    let entries = repo
        .get_entries_filtered(5000, 0, &qf)
        .map_err(|e| format!("query failed: {e}"))?;

    let mut sessions = build_session_groups(&entries);

    // Filter by prompt text if specified
    if let Some(search) = prompt_text {
        let lower = search.to_lowercase();
        sessions.retain(|s| s.first_prompt.to_lowercase().contains(&lower));
    }

    sessions.truncate(limit);

    if sessions.is_empty() {
        return Ok("No agent sessions found.".to_string());
    }

    let mut out = format!(
        "{} agent session{}:\n\n",
        sessions.len(),
        if sessions.len() == 1 { "" } else { "s" }
    );
    for (i, s) in sessions.iter().enumerate() {
        let rel = format_relative_time(s.last_command_at);
        let duration_mins = (s.last_command_at - s.first_command_at) / 60_000;
        let dur_str = if duration_mins < 1 {
            "< 1 min".to_string()
        } else {
            format!("{duration_mins} min")
        };
        let _ = writeln!(out, "{}. {} ({})", i + 1, s.session_id, s.executor);
        let _ = writeln!(
            out,
            "   {} | {} | {} commands | {} ok, {} failed",
            rel, dur_str, s.command_count, s.success_count, s.failure_count,
        );
        if !s.directories.is_empty() {
            let _ = writeln!(out, "   Directories: {}", s.directories.join(", "));
        }
        if !s.first_prompt.is_empty() {
            let prompt_display = if s.first_prompt.len() > 80 {
                format!("{}...", &s.first_prompt[..77])
            } else {
                s.first_prompt.clone()
            };
            let _ = writeln!(out, "   First prompt: \"{prompt_display}\"");
        }
        let _ = writeln!(out, "   Risk: {}", s.risk_summary);

        let rid = resume_id(&s.session_id);
        if s.session_id.starts_with("claude-") {
            let _ = writeln!(out, "   Resume: claude --resume {rid}");
        }
        out.push('\n');
    }
    Ok(out)
}

#[allow(clippy::too_many_lines)]
fn handle_replay_agent_session(repo: &Repository, args: &Value) -> Result<String, String> {
    let raw_id = get_str(args, "session_id").ok_or("session_id is required")?;
    let limit = usize::try_from(get_int(args, "limit", 100)).unwrap_or(100);

    // Normalize: try as-is, then with prefixes
    let session_id = {
        let filter = crate::repository::ReplayFilter {
            limit: Some(1),
            ..Default::default()
        };
        let try_ids = [
            raw_id.to_string(),
            format!("claude-{raw_id}"),
            format!("cursor-{raw_id}"),
        ];
        let mut found = None;
        for candidate in &try_ids {
            let entries = repo
                .get_replay_entries(Some(candidate), &filter)
                .unwrap_or_default();
            if !entries.is_empty() {
                found = Some(candidate.clone());
                break;
            }
        }
        found.ok_or_else(|| format!("No session found for '{raw_id}'"))?
    };

    let entries = repo
        .get_replay_entries(
            Some(&session_id),
            &crate::repository::ReplayFilter {
                limit: Some(limit),
                ..Default::default()
            },
        )
        .map_err(|e| format!("query failed: {e}"))?;

    if entries.is_empty() {
        return Err(format!("No commands found for session '{session_id}'"));
    }

    let executor = entries
        .first()
        .and_then(|e| e.executor.clone())
        .unwrap_or_else(|| "unknown".to_string());
    let total = entries.len();
    let success = entries.iter().filter(|e| e.exit_code == Some(0)).count();
    let rate = (success * 100).checked_div(total).unwrap_or(0);

    let mut out = String::new();
    let _ = writeln!(
        out,
        "Session {session_id} ({executor}) — {total} commands, {rate}% success\n",
    );
    let _ = writeln!(out, "Timeline:\n");

    let mut last_prompt = String::new();
    for entry in &entries {
        // Show prompt if it changed
        let prompt = entry
            .context
            .as_ref()
            .and_then(|ctx| ctx.get("agent_prompt"))
            .cloned()
            .unwrap_or_default();
        if !prompt.is_empty() && prompt != last_prompt {
            let _ = writeln!(out, "  [PROMPT] \"{prompt}\"");
            let _ = writeln!(out, "           {}\n", format_time(entry.started_at));
            last_prompt = prompt;
        }

        let exit = match entry.exit_code {
            Some(0) => "ok".to_string(),
            Some(c) => format!("exit {c}"),
            None => "?".to_string(),
        };
        let dur = util::format_duration_ms(entry.duration_ms);
        let _ = writeln!(out, "  [{exit:<7}] {}", entry.command);
        let _ = writeln!(
            out,
            "           {} | {} | {}\n",
            entry.cwd,
            dur,
            format_time(entry.started_at),
        );
    }

    // Summary
    let failure = entries
        .iter()
        .filter(|e| e.exit_code.is_some_and(|c| c != 0))
        .count();
    let mut dirs: Vec<String> = entries
        .iter()
        .map(|e| e.cwd.clone())
        .collect::<std::collections::HashSet<_>>()
        .into_iter()
        .collect();
    dirs.sort();

    let risk_data = crate::risk::session_risk(&entries);
    let mut risk_parts = Vec::new();
    if risk_data.critical_count > 0 {
        risk_parts.push(format!("{} critical", risk_data.critical_count));
    }
    if risk_data.high_count > 0 {
        risk_parts.push(format!("{} high", risk_data.high_count));
    }
    if risk_data.medium_count > 0 {
        risk_parts.push(format!("{} medium", risk_data.medium_count));
    }
    let safe = risk_data.safe_count + risk_data.low_count;
    if safe > 0 {
        risk_parts.push(format!("{safe} safe"));
    }
    let risk_str = if risk_parts.is_empty() {
        "all safe".to_string()
    } else {
        risk_parts.join(", ")
    };

    let first_at = entries.iter().map(|e| e.started_at).min().unwrap_or(0);
    let last_at = entries.iter().map(|e| e.ended_at).max().unwrap_or(0);
    let duration_mins = (last_at - first_at) / 60_000;
    let duration_str = if duration_mins < 1 {
        "< 1 minute".to_string()
    } else {
        format!(
            "{duration_mins} minute{}",
            if duration_mins == 1 { "" } else { "s" }
        )
    };

    let _ = writeln!(out, "Summary:");
    let _ = writeln!(out, "  Commands: {total} ({success} ok, {failure} failed)");
    let _ = writeln!(out, "  Duration: {duration_str}");
    let _ = writeln!(out, "  Directories: {}", dirs.join(", "));
    let _ = writeln!(out, "  Risk: {risk_str}");

    let rid = resume_id(&session_id);
    if session_id.starts_with("claude-") {
        let _ = writeln!(out, "  Resume: claude --resume {rid}");
    }

    Ok(out)
}

struct CmdFailStats {
    total: usize,
    fails: usize,
    agent_total: usize,
    agent_fails: usize,
    last_fail_at: i64,
}

fn handle_learn_from_failures(repo: &Repository, args: &Value) -> Result<String, String> {
    let days = get_int(args, "days", 7);
    let directory = get_str(args, "directory");

    let now = chrono::Utc::now().timestamp_millis();
    let after = now - days * 24 * 60 * 60 * 1000;

    let qf = QueryFilter {
        after: Some(after),
        cwd: directory,
        ..QueryFilter::default()
    };

    let entries = repo
        .get_entries_filtered(5000, 0, &qf)
        .map_err(|e| format!("query failed: {e}"))?;

    if entries.is_empty() {
        return Ok(format!("No command history in the last {days} days."));
    }

    // Group by command and compute failure stats
    let mut stats: std::collections::HashMap<&str, CmdFailStats> = std::collections::HashMap::new();
    for e in &entries {
        let entry = stats.entry(e.command.as_str()).or_insert(CmdFailStats {
            total: 0,
            fails: 0,
            agent_total: 0,
            agent_fails: 0,
            last_fail_at: 0,
        });
        entry.total += 1;
        let is_agent = e.executor_type.as_deref() == Some("agent");
        if is_agent {
            entry.agent_total += 1;
        }
        if e.exit_code.is_some_and(|c| c != 0) {
            entry.fails += 1;
            if is_agent {
                entry.agent_fails += 1;
            }
            if e.started_at > entry.last_fail_at {
                entry.last_fail_at = e.started_at;
            }
        }
    }

    // Filter to commands that fail frequently (3+ runs, 40%+ failure rate)
    let mut problem_cmds: Vec<_> = stats
        .into_iter()
        .filter(|(_, s)| s.total >= 3 && s.fails * 100 / s.total >= 40)
        .collect();

    problem_cmds.sort_by(|a, b| {
        let rate_a = a.1.fails * 100 / a.1.total;
        let rate_b = b.1.fails * 100 / b.1.total;
        rate_b.cmp(&rate_a).then(b.1.fails.cmp(&a.1.fails))
    });

    if problem_cmds.is_empty() {
        return Ok(format!(
            "No recurring failures in the last {days} days. All frequently-run commands have acceptable success rates."
        ));
    }

    let mut out = format!(
        "Recurring failures (last {days} days, {} problem commands):\n\n",
        problem_cmds.len()
    );

    for (cmd, s) in problem_cmds.iter().take(10) {
        let rate = s.fails * 100 / s.total;
        let display = util::truncate_str(cmd, 60, "...");
        let rel = format_relative_time(s.last_fail_at);
        let _ = writeln!(
            out,
            "  {display}\n    Failed {}/{} runs ({rate}%) — last failure {rel}",
            s.fails, s.total,
        );
        if s.agent_total > 0 && s.total > s.agent_total {
            let agent_rate = (s.agent_fails * 100)
                .checked_div(s.agent_total)
                .unwrap_or(0);
            let human_total = s.total - s.agent_total;
            let human_fails = s.fails - s.agent_fails;
            let human_rate = (human_fails * 100).checked_div(human_total).unwrap_or(0);
            let _ = writeln!(
                out,
                "    Agents: {agent_rate}% fail rate — Humans: {human_rate}% fail rate"
            );
        }
        out.push('\n');
    }

    Ok(out)
}

fn is_build_test_lint(cmd: &str) -> bool {
    cmd.starts_with("cargo test")
        || cmd.starts_with("cargo build")
        || cmd.starts_with("cargo clippy")
        || cmd.starts_with("npm test")
        || cmd.starts_with("npm run")
        || cmd.starts_with("pytest")
        || cmd.starts_with("go test")
        || cmd.starts_with("make")
}

fn format_build_test_lint(entries: &[crate::models::Entry], out: &mut String) {
    let btl: Vec<_> = entries
        .iter()
        .filter(|e| is_build_test_lint(&e.command))
        .collect();
    if btl.is_empty() {
        return;
    }
    let mut counts: std::collections::HashMap<&str, (usize, usize)> =
        std::collections::HashMap::new();
    for e in &btl {
        let entry = counts.entry(e.command.as_str()).or_insert((0, 0));
        entry.0 += 1;
        if e.exit_code == Some(0) {
            entry.1 += 1;
        }
    }
    let mut sorted: Vec<_> = counts.into_iter().collect();
    sorted.sort_by_key(|b| std::cmp::Reverse((b.1).0));

    let _ = writeln!(out, "  Build/test/lint commands:");
    for (cmd, (total, success)) in sorted.iter().take(5) {
        let rate = if *total > 0 { success * 100 / total } else { 0 };
        let display = util::truncate_str(cmd, 50, "...");
        let _ = writeln!(out, "    {display} — {total} runs, {rate}% success");
    }
    out.push('\n');
}

fn handle_project_context(repo: &Repository, args: &Value) -> Result<String, String> {
    let days = get_int(args, "days", 7);
    let directory = get_str(args, "directory");

    let now = chrono::Utc::now().timestamp_millis();
    let after = now - days * 24 * 60 * 60 * 1000;
    let day_ago = now - 24 * 60 * 60 * 1000;

    let qf = QueryFilter {
        after: Some(after),
        cwd: directory,
        ..QueryFilter::default()
    };

    let entries = repo
        .get_entries_filtered(5000, 0, &qf)
        .map_err(|e| format!("query failed: {e}"))?;

    if entries.is_empty() {
        return Ok(format!("No command history in the last {days} days."));
    }

    let dir_label = directory.unwrap_or("all directories");
    let mut out = format!(
        "Project context for {dir_label} (last {days} days, {} commands):\n\n",
        entries.len()
    );

    // Top commands by frequency
    let mut cmd_counts: std::collections::HashMap<&str, usize> = std::collections::HashMap::new();
    for e in &entries {
        let program = e.command.split_whitespace().next().unwrap_or(&e.command);
        *cmd_counts.entry(program).or_default() += 1;
    }
    let mut top_cmds: Vec<_> = cmd_counts.into_iter().collect();
    top_cmds.sort_by_key(|b| std::cmp::Reverse(b.1));
    top_cmds.truncate(10);

    let _ = writeln!(out, "  Common commands:");
    for (cmd, count) in &top_cmds {
        let _ = writeln!(out, "    {count:>4}x  {cmd}");
    }
    out.push('\n');

    format_build_test_lint(&entries, &mut out);

    // Recent failures (last 24h)
    let recent_failures: Vec<_> = entries
        .iter()
        .filter(|e| e.started_at >= day_ago && e.exit_code.is_some_and(|c| c != 0))
        .collect();
    if !recent_failures.is_empty() {
        let _ = writeln!(out, "  Recent failures (last 24h):");
        for e in recent_failures.iter().take(5) {
            let code = e.exit_code.unwrap_or(-1);
            let display = util::truncate_str(&e.command, 50, "...");
            let rel = format_relative_time(e.started_at);
            let _ = writeln!(out, "    exit {code} | {display} — {rel}");
        }
        out.push('\n');
    }

    // Agent activity
    let agent_sessions = build_session_groups(&entries);
    if !agent_sessions.is_empty() {
        let _ = writeln!(
            out,
            "  Agent sessions ({} this week):",
            agent_sessions.len()
        );
        for s in agent_sessions.iter().take(3) {
            let rel = format_relative_time(s.last_command_at);
            let _ = writeln!(
                out,
                "    {} ({}, {}) — {} cmds, {} failed",
                s.session_id, s.executor, rel, s.command_count, s.failure_count,
            );
            if !s.first_prompt.is_empty() {
                let display = util::truncate_str(&s.first_prompt, 60, "...");
                let _ = writeln!(out, "      \"{display}\"");
            }
        }
    }

    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn default_mcp() -> crate::config::McpConfig {
        crate::config::McpConfig::default()
    }

    #[test]
    fn test_list_tools_with_disabled() {
        let mut mcp = default_mcp();
        mcp.disabled_tools = vec!["assess_risk".to_string(), "suggest_next".to_string()];
        let resp = list_tools(&json!(1), &mcp);
        let tools = resp["result"]["tools"].as_array().unwrap();
        assert_eq!(tools.len(), 13);
        let names: Vec<&str> = tools.iter().map(|t| t["name"].as_str().unwrap()).collect();
        assert!(!names.contains(&"assess_risk"));
        assert!(!names.contains(&"suggest_next"));
        assert!(names.contains(&"search_commands"));
    }

    #[test]
    fn test_call_disabled_tool_returns_error() {
        let (_dir, repo) = crate::test_utils::test_repo();
        let mut mcp = default_mcp();
        mcp.disabled_tools = vec!["search_commands".to_string()];
        let result = call_tool(&repo, "search_commands", &json!({"query": "test"}), &mcp);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("disabled"));
    }

    #[test]
    fn test_list_tools_count() {
        let resp = list_tools(&json!(1), &default_mcp());
        let tools = resp["result"]["tools"].as_array().unwrap();
        assert_eq!(tools.len(), 15);

        let names: Vec<&str> = tools.iter().map(|t| t["name"].as_str().unwrap()).collect();
        assert!(names.contains(&"search_commands"));
        assert!(names.contains(&"recent_commands"));
        assert!(names.contains(&"command_status"));
        assert!(names.contains(&"get_prompts"));
        assert!(names.contains(&"session_history"));
        assert!(names.contains(&"get_stats"));
        assert!(names.contains(&"list_sessions"));
        assert!(names.contains(&"what_changed"));
        assert!(names.contains(&"what_failed"));
        assert!(names.contains(&"suggest_next"));
        assert!(names.contains(&"assess_risk"));
        assert!(names.contains(&"find_agent_session"));
        assert!(names.contains(&"replay_agent_session"));
        assert!(names.contains(&"learn_from_failures"));
        assert!(names.contains(&"project_context"));
    }

    #[test]
    fn test_tool_definitions_have_required_fields() {
        let resp = list_tools(&json!(1), &default_mcp());
        let tools = resp["result"]["tools"].as_array().unwrap();
        for tool in tools {
            assert!(tool["name"].is_string(), "tool missing name");
            assert!(tool["description"].is_string(), "tool missing description");
            assert!(tool["inputSchema"].is_object(), "tool missing inputSchema");
            assert_eq!(
                tool["inputSchema"]["type"], "object",
                "inputSchema must be object type"
            );
        }
    }

    #[test]
    fn test_call_unknown_tool() {
        let repo = crate::test_utils::test_repo().1;
        let result = call_tool(&repo, "nonexistent", &json!({}), &default_mcp());
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("Unknown tool"));
    }

    #[test]
    fn test_search_commands_empty_db() {
        let (_dir, repo) = crate::test_utils::test_repo();
        let result = call_tool(
            &repo,
            "search_commands",
            &json!({"query": "git"}),
            &default_mcp(),
        );
        assert!(result.is_ok());
        assert!(result.unwrap().contains("No commands found"));
    }

    #[test]
    fn test_recent_commands_empty_db() {
        let (_dir, repo) = crate::test_utils::test_repo();
        let result = call_tool(&repo, "recent_commands", &json!({}), &default_mcp());
        assert!(result.is_ok());
        assert!(result.unwrap().contains("No recent commands"));
    }

    #[test]
    fn test_command_status_requires_command() {
        let (_dir, repo) = crate::test_utils::test_repo();
        let result = call_tool(&repo, "command_status", &json!({}), &default_mcp());
        assert!(result.is_err());
    }

    #[test]
    fn test_list_sessions_empty_db() {
        let (_dir, repo) = crate::test_utils::test_repo();
        let result = call_tool(&repo, "list_sessions", &json!({}), &default_mcp());
        assert!(result.is_ok());
        assert!(result.unwrap().contains("No sessions found"));
    }

    #[test]
    fn test_get_prompts_empty_db() {
        let (_dir, repo) = crate::test_utils::test_repo();
        let result = call_tool(&repo, "get_prompts", &json!({}), &default_mcp());
        assert!(result.is_ok());
        assert!(result.unwrap().contains("No agent prompts"));
    }

    #[test]
    fn test_search_with_seeded_data() {
        let (_dir, repo) = crate::test_utils::test_repo();

        // Seed a session and entry
        let session = crate::models::Session {
            id: "test-sess".to_string(),
            hostname: "test".to_string(),
            created_at: 1_700_000_000_000,
            tag_id: None,
        };
        repo.insert_session(&session).unwrap();

        let entry = crate::models::Entry::new(
            "test-sess".to_string(),
            "cargo test".to_string(),
            "/project".to_string(),
            Some(0),
            1_700_000_000_000,
            1_700_000_001_000,
        );
        repo.insert_entry(&entry).unwrap();

        let result = call_tool(
            &repo,
            "search_commands",
            &json!({"query": "cargo"}),
            &default_mcp(),
        );
        assert!(result.is_ok());
        let text = result.unwrap();
        assert!(text.contains("cargo test"));
        assert!(text.contains("/project"));
    }

    #[test]
    fn test_search_commands_with_exit_code_filter() {
        let (_dir, repo) = crate::test_utils::test_repo();

        let session = crate::models::Session {
            id: "sess-exit".to_string(),
            hostname: "test".to_string(),
            created_at: 1_700_000_000_000,
            tag_id: None,
        };
        repo.insert_session(&session).unwrap();

        // Insert a passing command
        let ok_entry = crate::models::Entry::new(
            "sess-exit".to_string(),
            "cargo build".to_string(),
            "/project".to_string(),
            Some(0),
            1_700_000_000_000,
            1_700_000_001_000,
        );
        repo.insert_entry(&ok_entry).unwrap();

        // Insert a failing command
        let fail_entry = crate::models::Entry::new(
            "sess-exit".to_string(),
            "cargo build --release".to_string(),
            "/project".to_string(),
            Some(1),
            1_700_000_002_000,
            1_700_000_003_000,
        );
        repo.insert_entry(&fail_entry).unwrap();

        // Filter for exit_code = 0 (success only)
        let result = call_tool(
            &repo,
            "search_commands",
            &json!({"query": "cargo build", "exit_code": 0}),
            &default_mcp(),
        );
        assert!(result.is_ok());
        let text = result.unwrap();
        assert!(text.contains("cargo build"), "should find the command");
        // Only the successful entry should appear: "cargo build" (exit 0).
        // "cargo build --release" (exit 1) should be excluded.
        assert!(
            !text.contains("exit 1"),
            "should not contain failing command"
        );

        // Filter for exit_code = 1 (failure only)
        let result = call_tool(
            &repo,
            "search_commands",
            &json!({"query": "cargo build", "exit_code": 1}),
            &default_mcp(),
        );
        assert!(result.is_ok());
        let text = result.unwrap();
        assert!(text.contains("cargo build --release"));
        assert!(text.contains("exit 1"));
    }

    #[test]
    fn test_command_status_with_seeded_data() {
        let (_dir, repo) = crate::test_utils::test_repo();

        let session = crate::models::Session {
            id: "sess-status".to_string(),
            hostname: "test".to_string(),
            created_at: 1_700_000_000_000,
            tag_id: None,
        };
        repo.insert_session(&session).unwrap();

        // Insert two runs of "make test": one pass, one fail
        let pass_entry = crate::models::Entry::new(
            "sess-status".to_string(),
            "make test".to_string(),
            "/project".to_string(),
            Some(0),
            1_700_000_000_000,
            1_700_000_001_000,
        );
        repo.insert_entry(&pass_entry).unwrap();

        let fail_entry = crate::models::Entry::new(
            "sess-status".to_string(),
            "make test".to_string(),
            "/project".to_string(),
            Some(2),
            1_700_000_002_000,
            1_700_000_003_000,
        );
        repo.insert_entry(&fail_entry).unwrap();

        let result = call_tool(
            &repo,
            "command_status",
            &json!({"command": "make test"}),
            &default_mcp(),
        );
        assert!(result.is_ok());
        let text = result.unwrap();
        assert!(
            text.contains("1 succeeded"),
            "should report 1 success: {text}"
        );
        assert!(text.contains("1 failed"), "should report 1 failure: {text}");
        assert!(text.contains("make test"), "should contain the command");
    }

    #[test]
    fn test_get_prompts_with_seeded_data() {
        let (_dir, repo) = crate::test_utils::test_repo();

        let session = crate::models::Session {
            id: "claude-prompt-sess".to_string(),
            hostname: "test".to_string(),
            created_at: 1_700_000_000_000,
            tag_id: None,
        };
        repo.insert_session(&session).unwrap();

        // Insert an agent entry with a prompt in context
        let mut context = std::collections::HashMap::new();
        context.insert("agent_prompt".to_string(), "fix the tests".to_string());

        let mut entry = crate::models::Entry::new(
            "claude-prompt-sess".to_string(),
            "cargo test".to_string(),
            "/project".to_string(),
            Some(0),
            1_700_000_000_000,
            1_700_000_001_000,
        );
        entry.context = Some(context);
        entry.executor_type = Some("agent".to_string());
        entry.executor = Some("claude-code".to_string());
        repo.insert_entry(&entry).unwrap();

        let result = call_tool(&repo, "get_prompts", &json!({}), &default_mcp());
        assert!(result.is_ok());
        let text = result.unwrap();
        assert!(
            text.contains("fix the tests"),
            "should contain the prompt: {text}"
        );
        assert!(
            text.contains("cargo test"),
            "should contain the command: {text}"
        );
    }

    // ── Smart tool tests ────────────────────────────────────

    #[test]
    fn test_what_changed_empty_db() {
        let (_dir, repo) = crate::test_utils::test_repo();
        let result = call_tool(&repo, "what_changed", &json!({}), &default_mcp());
        assert!(result.is_ok());
        assert!(result.unwrap().contains("No file-modifying operations"));
    }

    #[test]
    fn test_what_changed_classifies_commands() {
        let (_dir, repo) = crate::test_utils::test_repo();
        let session = crate::models::Session {
            id: "s1".to_string(),
            hostname: "test".to_string(),
            created_at: chrono::Utc::now().timestamp_millis(),
            tag_id: None,
        };
        repo.insert_session(&session).unwrap();

        let now = chrono::Utc::now().timestamp_millis();
        for (i, cmd) in ["rm -rf tmp/", "git commit -m 'fix'", "npm install express"]
            .iter()
            .enumerate()
        {
            let entry = crate::models::Entry::new(
                "s1".to_string(),
                cmd.to_string(),
                "/project".to_string(),
                Some(0),
                now - (i as i64 * 1000),
                now - (i as i64 * 1000) + 100,
            );
            repo.insert_entry(&entry).unwrap();
        }

        let result = call_tool(&repo, "what_changed", &json!({"hours": 1}), &default_mcp());
        assert!(result.is_ok());
        let text = result.unwrap();
        assert!(text.contains("DELETIONS"), "should classify rm: {text}");
        assert!(
            text.contains("GIT COMMITS"),
            "should classify git commit: {text}"
        );
        assert!(
            text.contains("PACKAGE INSTALLS"),
            "should classify npm install: {text}"
        );
    }

    #[test]
    fn test_what_failed_empty_db() {
        let (_dir, repo) = crate::test_utils::test_repo();
        let result = call_tool(&repo, "what_failed", &json!({}), &default_mcp());
        assert!(result.is_ok());
        assert!(result.unwrap().contains("No failures"));
    }

    #[test]
    fn test_what_failed_with_failures() {
        let (_dir, repo) = crate::test_utils::test_repo();
        let session = crate::models::Session {
            id: "s1".to_string(),
            hostname: "test".to_string(),
            created_at: chrono::Utc::now().timestamp_millis(),
            tag_id: None,
        };
        repo.insert_session(&session).unwrap();

        let now = chrono::Utc::now().timestamp_millis();
        let mut entry = crate::models::Entry::new(
            "s1".to_string(),
            "cargo test".to_string(),
            "/project".to_string(),
            Some(1),
            now - 1000,
            now,
        );
        let mut ctx = std::collections::HashMap::new();
        ctx.insert("agent_prompt".to_string(), "run the tests".to_string());
        entry.context = Some(ctx);
        entry.executor_type = Some("agent".to_string());
        entry.executor = Some("claude-code".to_string());
        repo.insert_entry(&entry).unwrap();

        let result = call_tool(&repo, "what_failed", &json!({}), &default_mcp());
        assert!(result.is_ok());
        let text = result.unwrap();
        assert!(text.contains("1 failure"), "should count failure: {text}");
        assert!(text.contains("run the tests"), "should show prompt: {text}");
        assert!(text.contains("cargo test"), "should show command: {text}");
    }

    #[test]
    fn test_suggest_next_empty_db() {
        let (_dir, repo) = crate::test_utils::test_repo();
        let result = call_tool(&repo, "suggest_next", &json!({}), &default_mcp());
        assert!(result.is_ok());
        assert!(result.unwrap().contains("No recent commands"));
    }

    #[test]
    fn test_suggest_next_with_data() {
        let (_dir, repo) = crate::test_utils::test_repo();
        let session = crate::models::Session {
            id: "s1".to_string(),
            hostname: "test".to_string(),
            created_at: chrono::Utc::now().timestamp_millis(),
            tag_id: None,
        };
        repo.insert_session(&session).unwrap();

        let now = chrono::Utc::now().timestamp_millis();
        // Run "cargo test" 5 times, "ls" once
        for i in 0..5 {
            let entry = crate::models::Entry::new(
                "s1".to_string(),
                "cargo test".to_string(),
                "/project".to_string(),
                Some(0),
                now - (i * 60_000),
                now - (i * 60_000) + 100,
            );
            repo.insert_entry(&entry).unwrap();
        }
        let entry = crate::models::Entry::new(
            "s1".to_string(),
            "ls".to_string(),
            "/project".to_string(),
            Some(0),
            now - 300_000,
            now - 300_000 + 100,
        );
        repo.insert_entry(&entry).unwrap();

        let result = call_tool(&repo, "suggest_next", &json!({}), &default_mcp());
        assert!(result.is_ok());
        let text = result.unwrap();
        assert!(
            text.contains("cargo test"),
            "should suggest cargo test: {text}"
        );
        // cargo test should be ranked higher than ls (more frequent + recent)
        let cargo_pos = text.find("cargo test").unwrap();
        let ls_pos = text.find("ls").unwrap();
        assert!(
            cargo_pos < ls_pos,
            "cargo test should rank above ls: {text}"
        );
    }

    #[test]
    fn test_classify_command() {
        assert_eq!(classify_command("rm -rf /tmp"), Some("deletions"));
        assert_eq!(classify_command("mv a.txt b.txt"), Some("moves/renames"));
        assert_eq!(classify_command("git commit -m 'fix'"), Some("git commits"));
        assert_eq!(classify_command("git push origin main"), Some("git sync"));
        assert_eq!(
            classify_command("npm install express"),
            Some("package installs")
        );
        assert_eq!(
            classify_command("chmod 755 script.sh"),
            Some("permission changes")
        );
        assert_eq!(classify_command("ls -la"), None);
        assert_eq!(classify_command("cat file.txt"), None);
        assert_eq!(classify_command("grep TODO src/"), None);
    }

    // ── assess_risk tests ───────────────────────────────────

    #[test]
    fn test_assess_risk_critical_command() {
        let result = handle_assess_risk(&json!({"command": "rm -rf /"}));
        assert!(result.is_ok());
        let text = result.unwrap();
        assert!(text.contains("CRITICAL"), "should be critical: {text}");
    }

    #[test]
    fn test_assess_risk_safe_command() {
        let result = handle_assess_risk(&json!({"command": "git status"}));
        assert!(result.is_ok());
        let text = result.unwrap();
        assert!(
            text.contains("SAFE") || text.contains("No known risk"),
            "should be safe: {text}"
        );
    }

    #[test]
    fn test_assess_risk_high_command() {
        let result = handle_assess_risk(&json!({"command": "npm install some-package"}));
        assert!(result.is_ok());
        let text = result.unwrap();
        assert!(
            text.contains("HIGH") || text.contains("MEDIUM"),
            "package install should be high/medium risk: {text}"
        );
    }

    #[test]
    fn test_assess_risk_batch() {
        let result = handle_assess_risk(&json!({
            "commands": ["git status", "rm -rf /tmp", "ls"]
        }));
        assert!(result.is_ok());
        let text = result.unwrap();
        assert!(
            text.contains("3 commands"),
            "should assess 3 commands: {text}"
        );
        assert!(
            text.contains("CRITICAL") || text.contains("HIGH"),
            "should detect risky command: {text}"
        );
    }

    #[test]
    fn test_assess_risk_no_input() {
        let result = handle_assess_risk(&json!({}));
        assert!(result.is_err());
    }

    #[test]
    fn test_assess_risk_force_push() {
        let result = handle_assess_risk(&json!({"command": "git push --force origin main"}));
        assert!(result.is_ok());
        let text = result.unwrap();
        assert!(
            text.contains("CRITICAL") || text.contains("HIGH"),
            "force push should be high/critical: {text}"
        );
    }

    // ── Agent session tool tests ────────────────────────────

    fn seed_agent_sessions(repo: &Repository) {
        use std::collections::HashMap;

        let now = chrono::Utc::now().timestamp_millis();

        // Session 1: claude-code, 3 commands, 1 failure
        let s1 = crate::models::Session {
            id: "claude-abc123".into(),
            hostname: "test".into(),
            created_at: now - 3_600_000,
            tag_id: None,
        };
        repo.insert_session(&s1).unwrap();

        let mut e1 = crate::models::Entry::new(
            "claude-abc123".into(),
            "grep -r 'auth' src/".into(),
            "/project".into(),
            Some(0),
            now - 3_600_000,
            now - 3_599_900,
        );
        let mut ctx1 = HashMap::new();
        ctx1.insert("agent_prompt".into(), "refactor auth module".into());
        e1.context = Some(ctx1);
        e1.executor_type = Some("agent".into());
        e1.executor = Some("claude-code".into());
        repo.insert_entry(&e1).unwrap();

        let mut e2 = crate::models::Entry::new(
            "claude-abc123".into(),
            "npm test".into(),
            "/project".into(),
            Some(1),
            now - 3_590_000,
            now - 3_580_000,
        );
        let mut ctx2 = HashMap::new();
        ctx2.insert("agent_prompt".into(), "refactor auth module".into());
        e2.context = Some(ctx2);
        e2.executor_type = Some("agent".into());
        e2.executor = Some("claude-code".into());
        repo.insert_entry(&e2).unwrap();

        let mut e3 = crate::models::Entry::new(
            "claude-abc123".into(),
            "cargo build".into(),
            "/project".into(),
            Some(0),
            now - 3_570_000,
            now - 3_560_000,
        );
        let mut ctx3 = HashMap::new();
        ctx3.insert("agent_prompt".into(), "fix the build".into());
        e3.context = Some(ctx3);
        e3.executor_type = Some("agent".into());
        e3.executor = Some("claude-code".into());
        repo.insert_entry(&e3).unwrap();

        // Session 2: cursor, 2 commands, all ok, different directory
        let s2 = crate::models::Session {
            id: "cursor-def456".into(),
            hostname: "test".into(),
            created_at: now - 7_200_000,
            tag_id: None,
        };
        repo.insert_session(&s2).unwrap();

        let mut e4 = crate::models::Entry::new(
            "cursor-def456".into(),
            "git status".into(),
            "/other-project".into(),
            Some(0),
            now - 7_200_000,
            now - 7_199_900,
        );
        let mut ctx4 = HashMap::new();
        ctx4.insert("agent_prompt".into(), "check git status".into());
        e4.context = Some(ctx4);
        e4.executor_type = Some("agent".into());
        e4.executor = Some("cursor".into());
        repo.insert_entry(&e4).unwrap();

        let mut e5 = crate::models::Entry::new(
            "cursor-def456".into(),
            "git add .".into(),
            "/other-project".into(),
            Some(0),
            now - 7_190_000,
            now - 7_189_900,
        );
        e5.executor_type = Some("agent".into());
        e5.executor = Some("cursor".into());
        repo.insert_entry(&e5).unwrap();
    }

    #[test]
    fn test_find_agent_session_empty_db() {
        let (_dir, repo) = crate::test_utils::test_repo();
        let result = call_tool(&repo, "find_agent_session", &json!({}), &default_mcp());
        assert!(result.is_ok());
        assert!(
            result.unwrap().contains("No agent sessions found"),
            "should report no sessions"
        );
    }

    #[test]
    fn test_find_agent_session_with_data() {
        let (_dir, repo) = crate::test_utils::test_repo();
        seed_agent_sessions(&repo);
        let result = call_tool(&repo, "find_agent_session", &json!({}), &default_mcp());
        assert!(result.is_ok());
        let text = result.unwrap();
        assert!(
            text.contains("claude-abc123"),
            "should find claude session: {text}"
        );
        assert!(
            text.contains("cursor-def456"),
            "should find cursor session: {text}"
        );
        assert!(
            text.contains("2 agent sessions"),
            "should count sessions: {text}"
        );
    }

    #[test]
    fn test_find_agent_session_filter_by_directory() {
        let (_dir, repo) = crate::test_utils::test_repo();
        seed_agent_sessions(&repo);
        let result = call_tool(
            &repo,
            "find_agent_session",
            &json!({"directory": "/project"}),
            &default_mcp(),
        );
        assert!(result.is_ok());
        let text = result.unwrap();
        assert!(
            text.contains("claude-abc123"),
            "should find claude session: {text}"
        );
        assert!(
            !text.contains("cursor-def456"),
            "should NOT find cursor session: {text}"
        );
    }

    #[test]
    fn test_find_agent_session_filter_by_executor() {
        let (_dir, repo) = crate::test_utils::test_repo();
        seed_agent_sessions(&repo);
        let result = call_tool(
            &repo,
            "find_agent_session",
            &json!({"executor": "cursor"}),
            &default_mcp(),
        );
        assert!(result.is_ok());
        let text = result.unwrap();
        assert!(
            text.contains("cursor-def456"),
            "should find cursor session: {text}"
        );
        assert!(
            !text.contains("claude-abc123"),
            "should NOT find claude session: {text}"
        );
    }

    #[test]
    fn test_find_agent_session_filter_by_prompt() {
        let (_dir, repo) = crate::test_utils::test_repo();
        seed_agent_sessions(&repo);
        // First verify entries exist by checking without prompt filter
        let all = call_tool(&repo, "find_agent_session", &json!({}), &default_mcp());
        assert!(all.is_ok());
        let all_text = all.unwrap();
        assert!(
            all_text.contains("claude-abc123"),
            "baseline: sessions should exist: {all_text}"
        );

        // Now test with prompt filter
        let result = call_tool(
            &repo,
            "find_agent_session",
            &json!({"prompt_text": "auth"}),
            &default_mcp(),
        );
        assert!(result.is_ok());
        let text = result.unwrap();
        assert!(
            text.contains("claude-abc123"),
            "should find session with auth prompt: {text}"
        );
        assert!(
            !text.contains("cursor-def456"),
            "should NOT find cursor session: {text}"
        );
    }

    #[test]
    fn test_find_agent_session_shows_resume() {
        let (_dir, repo) = crate::test_utils::test_repo();
        seed_agent_sessions(&repo);
        let result = call_tool(
            &repo,
            "find_agent_session",
            &json!({"executor": "claude-code"}),
            &default_mcp(),
        );
        assert!(result.is_ok());
        let text = result.unwrap();
        assert!(
            text.contains("claude --resume abc123"),
            "should show resume command: {text}"
        );
    }

    #[test]
    fn test_replay_agent_session_not_found() {
        let (_dir, repo) = crate::test_utils::test_repo();
        let result = call_tool(
            &repo,
            "replay_agent_session",
            &json!({"session_id": "nonexistent"}),
            &default_mcp(),
        );
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("No session found"));
    }

    #[test]
    fn test_replay_agent_session_with_data() {
        let (_dir, repo) = crate::test_utils::test_repo();
        seed_agent_sessions(&repo);
        let result = call_tool(
            &repo,
            "replay_agent_session",
            &json!({"session_id": "claude-abc123"}),
            &default_mcp(),
        );
        assert!(result.is_ok());
        let text = result.unwrap();
        assert!(
            text.contains("claude-abc123"),
            "should show session id: {text}"
        );
        assert!(
            text.contains("3 commands"),
            "should show command count: {text}"
        );
        assert!(text.contains("[PROMPT]"), "should show prompts: {text}");
        assert!(
            text.contains("refactor auth module"),
            "should show prompt text: {text}"
        );
        assert!(text.contains("grep"), "should show commands: {text}");
        assert!(text.contains("npm test"), "should show commands: {text}");
        assert!(text.contains("Summary:"), "should show summary: {text}");
    }

    #[test]
    fn test_replay_agent_session_prefix_normalization() {
        let (_dir, repo) = crate::test_utils::test_repo();
        seed_agent_sessions(&repo);
        // Pass without prefix — should find claude-abc123
        let result = call_tool(
            &repo,
            "replay_agent_session",
            &json!({"session_id": "abc123"}),
            &default_mcp(),
        );
        assert!(result.is_ok());
        let text = result.unwrap();
        assert!(
            text.contains("claude-abc123"),
            "should resolve prefix: {text}"
        );
    }

    #[test]
    fn test_replay_agent_session_shows_resume() {
        let (_dir, repo) = crate::test_utils::test_repo();
        seed_agent_sessions(&repo);
        let result = call_tool(
            &repo,
            "replay_agent_session",
            &json!({"session_id": "claude-abc123"}),
            &default_mcp(),
        );
        assert!(result.is_ok());
        let text = result.unwrap();
        assert!(
            text.contains("claude --resume abc123"),
            "should show resume command: {text}"
        );
    }

    #[test]
    fn test_replay_agent_session_requires_session_id() {
        let (_dir, repo) = crate::test_utils::test_repo();
        let result = call_tool(&repo, "replay_agent_session", &json!({}), &default_mcp());
        assert!(result.is_err());
    }

    // ── learn_from_failures tests ───────────────────────────

    #[test]
    fn test_learn_from_failures_empty_db() {
        let (_dir, repo) = crate::test_utils::test_repo();
        let result = call_tool(&repo, "learn_from_failures", &json!({}), &default_mcp());
        assert!(result.is_ok());
        assert!(result.unwrap().contains("No command history"));
    }

    #[test]
    fn test_learn_from_failures_detects_recurring() {
        let (_dir, repo) = crate::test_utils::test_repo();
        seed_agent_sessions(&repo);

        // Add more failures for "npm test" to trigger the 40% threshold
        let now = chrono::Utc::now().timestamp_millis();
        let session = &crate::models::Session {
            id: "claude-fail1".into(),
            hostname: "test".into(),
            created_at: now - 100_000,
            tag_id: None,
        };
        repo.insert_session(session).unwrap();

        for i in 0..5 {
            let mut e = crate::models::Entry::new(
                "claude-fail1".into(),
                "npm test".into(),
                "/project".into(),
                Some(1), // all fail
                now - (i * 10_000) - 50_000,
                now - (i * 10_000) - 49_000,
            );
            e.executor_type = Some("agent".into());
            e.executor = Some("claude-code".into());
            repo.insert_entry(&e).unwrap();
        }

        let result = call_tool(&repo, "learn_from_failures", &json!({}), &default_mcp());
        assert!(result.is_ok());
        let text = result.unwrap();
        assert!(
            text.contains("npm test"),
            "should detect npm test as recurring failure: {text}"
        );
        assert!(
            text.contains("5/5") || text.contains("100%"),
            "should show failure rate: {text}"
        );
    }

    #[test]
    fn test_learn_from_failures_no_problems() {
        let (_dir, repo) = crate::test_utils::test_repo();
        let now = chrono::Utc::now().timestamp_millis();

        let session = crate::models::Session {
            id: "s1".into(),
            hostname: "test".into(),
            created_at: now - 100_000,
            tag_id: None,
        };
        repo.insert_session(&session).unwrap();

        // All successful commands
        for i in 0..5 {
            let e = crate::models::Entry::new(
                "s1".into(),
                "git status".into(),
                "/project".into(),
                Some(0),
                now - (i * 10_000) - 50_000,
                now - (i * 10_000) - 49_000,
            );
            repo.insert_entry(&e).unwrap();
        }

        let result = call_tool(&repo, "learn_from_failures", &json!({}), &default_mcp());
        assert!(result.is_ok());
        assert!(
            result.unwrap().contains("No recurring failures"),
            "should report no problems"
        );
    }

    // ── project_context tests ───────────────────────────────

    #[test]
    fn test_project_context_empty_db() {
        let (_dir, repo) = crate::test_utils::test_repo();
        let result = call_tool(&repo, "project_context", &json!({}), &default_mcp());
        assert!(result.is_ok());
        assert!(result.unwrap().contains("No command history"));
    }

    #[test]
    fn test_project_context_with_data() {
        let (_dir, repo) = crate::test_utils::test_repo();
        seed_agent_sessions(&repo);

        let result = call_tool(&repo, "project_context", &json!({}), &default_mcp());
        assert!(result.is_ok());
        let text = result.unwrap();
        assert!(
            text.contains("Common commands"),
            "should show common commands: {text}"
        );
        assert!(
            text.contains("Agent sessions"),
            "should show agent sessions: {text}"
        );
    }

    #[test]
    fn test_project_context_filter_by_directory() {
        let (_dir, repo) = crate::test_utils::test_repo();
        seed_agent_sessions(&repo);

        let result = call_tool(
            &repo,
            "project_context",
            &json!({"directory": "/other-project"}),
            &default_mcp(),
        );
        assert!(result.is_ok());
        let text = result.unwrap();
        assert!(
            text.contains("git status") || text.contains("git add"),
            "should show cursor commands from /other-project: {text}"
        );
        assert!(
            !text.contains("grep"),
            "should NOT show claude commands from /project: {text}"
        );
    }
}
