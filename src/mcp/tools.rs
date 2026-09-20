//! The MCP tool surface. Every handler here renders its answer through
//! [`crate::mcp::conventions`] — one timestamp format, one token for a
//! missing value, one pagination contract, one way to say a row was
//! elided, and one line declaring whether the content is a record suvadu
//! stored or something it worked out. See that module for the rules and
//! [`crate::mcp::contract`] for the fixtures that hold them in place.

use std::fmt::Write;

use serde_json::{json, Value};

use super::conventions as conv;
use crate::models::SearchField;
use crate::repository::{QueryFilter, Repository};
use crate::util;

/// Maximum number of replay entries to fetch when grouping prompts.
const MAX_PROMPT_ENTRIES: usize = 5000;

/// Bounded samples the aggregating tools rank over. Each response states
/// the sample it used, per rule 5: a top-10 over 200 rows must never read
/// as a top-10 over the whole history.
const STATS_SAMPLE: usize = 200;
const FRECENCY_SAMPLE: usize = 2000;
const ANALYSIS_SAMPLE: usize = 5000;

/// The JSON-RPC definition of one catalog tool.
///
/// Deliberately an explicit `match`, not a table of function pointers: a
/// name in the catalog with no arm here is a compile-time-visible gap that
/// `catalog_tools_all_have_definitions` fails on, and nothing outside the
/// catalog can be advertised.
fn tool_definition(name: &str) -> Option<Value> {
    Some(match name {
        "list_agent_sessions" => super::ai_sessions::list_definition(),
        "get_agent_session" => super::ai_sessions::get_definition(),
        "resolve_current_agent_session" => super::ai_sessions::resolve_definition(),
        "save_session_summary" => super::ai_sessions::save_definition(),
        "search_commands" => search_commands_def(),
        "recent_commands" => recent_commands_def(),
        "command_status" => command_status_def(),
        "get_prompts" => get_prompts_def(),
        "session_history" => session_history_def(),
        "get_stats" => get_stats_def(),
        "list_sessions" => list_sessions_def(),
        "what_changed" => what_changed_def(),
        "what_failed" => what_failed_def(),
        "suggest_next" => suggest_next_def(),
        "assess_risk" => assess_risk_def(),
        "find_agent_session" => find_agent_session_def(),
        "replay_agent_session" => replay_agent_session_def(),
        "learn_from_failures" => learn_from_failures_def(),
        "project_context" => project_context_def(),
        "list_skills" => list_skills_def(),
        "get_skill" => get_skill_def(),
        "search_skills" => search_skills_def(),
        "propose_skill" => propose_skill_def(),
        _ => return None,
    })
}

/// Return the `tools/list` response.
///
/// Driven by `catalog::TOOLS` so the advertised set and the settings UI
/// cannot drift: a tool appears only when its effective state (write opt-in
/// satisfied, not in `mcp.disabled_tools`) is available.
pub fn list_tools(id: &Value, mcp: &crate::config::McpConfig) -> Value {
    let tools: Vec<Value> = super::catalog::TOOLS
        .iter()
        .filter(|entry| super::catalog::tool_state(entry, mcp).is_available())
        .filter_map(|entry| tool_definition(entry.name))
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
    // One gate for both ways a tool can be unavailable (explicitly disabled,
    // or a write whose opt-in is off), so an unadvertised tool can never be
    // invoked directly either. Handlers keep their own guards as well.
    let state = super::catalog::tool_state_by_name(name, mcp);
    if !state.is_available() && super::catalog::find_tool(name).is_some() {
        let reason = state.reason().unwrap_or_else(|| "disabled".to_string());
        return Err(format!("Tool '{name}' is disabled: {reason}"));
    }
    match name {
        "list_agent_sessions" => super::ai_sessions::list(repo, args, mcp),
        "get_agent_session" => super::ai_sessions::get(repo, args, mcp),
        "resolve_current_agent_session" => super::ai_sessions::resolve(repo, args, mcp),
        "save_session_summary" => super::ai_sessions::save(repo, args, mcp),
        "search_commands" => handle_search_commands(repo, args, mcp),
        "recent_commands" => handle_recent_commands(repo, args, mcp),
        "command_status" => handle_command_status(repo, args, mcp),
        "get_prompts" => handle_get_prompts(repo, args, mcp),
        "session_history" => handle_session_history(repo, args, mcp),
        "get_stats" => handle_get_stats(repo, args, mcp),
        "list_sessions" => handle_list_sessions(repo, args, mcp),
        "what_changed" => handle_what_changed(repo, args, mcp),
        "what_failed" => handle_what_failed(repo, args, mcp),
        "suggest_next" => handle_suggest_next(repo, args, mcp),
        "assess_risk" => handle_assess_risk(args),
        "find_agent_session" => handle_find_agent_session(repo, args, mcp),
        "replay_agent_session" => handle_replay_agent_session(repo, args, mcp),
        "learn_from_failures" => handle_learn_from_failures(repo, args, mcp),
        "project_context" => handle_project_context(repo, args, mcp),
        "list_skills" => handle_list_skills(repo, args, mcp),
        "get_skill" => handle_get_skill(repo, args, mcp),
        "search_skills" => handle_search_skills(repo, args, mcp),
        "propose_skill" => handle_propose_skill(args, mcp),
        _ => Err(format!("Unknown tool: {name}")),
    }
}

// ── Tool definitions ────────────────────────────────────────

fn search_commands_def() -> Value {
    json!({
        "name": "search_commands",
        "description": "Search recorded shell commands by text. One row per command: stable ID, exit code, timestamp, directory. Use this when you want individual commands — 'did I ever run X', 'what broke here', 'how is this project built'. When the question is about a whole piece of work rather than one command ('what did the last agent do', 'what was I working on'), start with find_agent_session or list_agent_sessions instead and drill in from there. Suvadu recorded the command and its exit code, never its output.",
        "inputSchema": {
            "type": "object",
            "properties": with_paging(json!({
                "query": { "type": "string", "description": "Text to search for in commands" },
                "directory": { "type": "string", "description": "Filter to commands run in this directory" },
                "executor": { "type": "string", "description": "Filter by executor (e.g. claude-code, cursor, human)" },
                "exit_code": { "type": "integer", "description": "Filter by exit code (0 = success)" },
                "after": { "type": "string", "description": "Start date (e.g. today, yesterday, 7 days ago, 2026-01-01)" },
                "before": { "type": "string", "description": "End date (e.g. today, yesterday, 7 days ago, 2026-01-01)" }
            }), 20),
            "required": ["query"]
        }
    })
}

fn recent_commands_def() -> Value {
    json!({
        "name": "recent_commands",
        "description": "The most recently recorded commands, newest first, optionally scoped to a directory or executor. Good for 'what just happened here'. For 'what is this session doing' call resolve_current_agent_session first — recent_commands has no notion of which session is yours and will happily return another project's work.",
        "inputSchema": {
            "type": "object",
            "properties": with_paging(json!({
                "directory": { "type": "string", "description": "Filter to commands run in this directory" },
                "executor": { "type": "string", "description": "Filter by executor (e.g. claude-code, cursor)" },
                "after": { "type": "string", "description": "Start date (e.g. today, yesterday, 7 days ago, 2026-01-01)" }
            }), 20)
        }
    })
}

fn command_status_def() -> Value {
    json!({
        "name": "command_status",
        "description": "Whether a specific command has been run before and how those runs ended. Returns previous runs with exit codes and timestamps, so you can see whether it usually succeeds. The exit code is all suvadu has: it never captured what the command printed.",
        "inputSchema": {
            "type": "object",
            "properties": with_paging(json!({
                "command": { "type": "string", "description": "Command text to search for (prefix match)" },
                "directory": { "type": "string", "description": "Filter to this directory" }
            }), 5),
            "required": ["command"]
        }
    })
}

fn get_prompts_def() -> Value {
    json!({
        "name": "get_prompts",
        "description": "Agent prompts recorded alongside the commands they were running under, grouped prompt by prompt. Use it to see which instruction led to which commands. This reads the prompt text stored on each command record, not an agent's transcript — for the transcript of a captured session use get_agent_session.",
        "inputSchema": {
            "type": "object",
            "properties": with_paging(json!({
                "executor": { "type": "string", "description": "Filter by executor (e.g. claude-code, cursor)" },
                "session_id": { "type": "string", "description": "Filter to a specific session" },
                "after": { "type": "string", "description": "Start date" }
            }), 10)
        }
    })
}

fn session_history_def() -> Value {
    json!({
        "name": "session_history",
        "description": "Every recorded command of one shell session, oldest first. Pass a session_id you already have (from list_sessions, find_agent_session or resolve_current_agent_session); with none it falls back to whatever session is most recent, which may not be yours.",
        "inputSchema": {
            "type": "object",
            "properties": with_paging(json!({
                "session_id": { "type": "string", "description": "Session ID (defaults to most recent — pass one explicitly to be sure)" }
            }), 50)
        }
    })
}

fn get_stats_def() -> Value {
    json!({
        "name": "get_stats",
        "description": "Aggregate counts over recorded commands: totals, success fraction, and the most frequent commands and directories. Rankings are computed from a bounded recent sample, which the response states.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "days": { "type": "integer", "description": "Time window in days (default: 7)", "default": 7 },
                "directory": { "type": "string", "description": "Filter to this directory and its subtree" }
            }
        }
    })
}

fn list_sessions_def() -> Value {
    json!({
        "name": "list_sessions",
        "description": "Recent shell sessions with command counts, time ranges and tags. These are terminal sessions, not agent sessions: for work an AI agent did, use find_agent_session (grouped from recorded commands) or list_agent_sessions (captured from the agent's own transcript).",
        "inputSchema": {
            "type": "object",
            "properties": with_paging(json!({
                "tag": { "type": "string", "description": "Filter by tag name" }
            }), 10)
        }
    })
}

fn what_changed_def() -> Value {
    json!({
        "name": "what_changed",
        "description": "What file-modifying work was attempted in a directory recently. Returns the recorded commands and their exit codes (observed), then a category breakdown — writes, deletions, git, installs — inferred from the command text of the ones that exited 0. Suvadu never watched the filesystem and never captured output, so a category is a reading of the command, not a confirmed change.",
        "inputSchema": {
            "type": "object",
            "properties": with_paging(json!({
                "directory": { "type": "string", "description": "Directory to analyze, including its subtree (defaults to all)" },
                "hours": { "type": "integer", "description": "How many hours back to look (default: 4)", "default": 4 },
                "executor": { "type": "string", "description": "Filter by executor (e.g. claude-code, cursor)" }
            }), 500)
        }
    })
}

fn what_failed_def() -> Value {
    json!({
        "name": "what_failed",
        "description": "Recently recorded commands that exited non-zero, grouped by the prompt they ran under where one was recorded. Tells you what was attempted and failed; it cannot tell you why, because the error text was never captured.",
        "inputSchema": {
            "type": "object",
            "properties": with_paging(json!({
                "directory": { "type": "string", "description": "Filter to this directory" },
                "hours": { "type": "integer", "description": "How many hours back to look (default: 24)", "default": 24 }
            }), 20)
        }
    })
}

fn suggest_next_def() -> Value {
    json!({
        "name": "suggest_next",
        "description": "Commands that are statistically likely next here, ranked by frecency (frequency plus recency) over recent history. A guess about habit, not a recommendation: it says nothing about whether running one now is correct or safe.",
        "inputSchema": {
            "type": "object",
            "properties": with_paging(json!({
                "directory": { "type": "string", "description": "Directory context (defaults to all)" }
            }), 10)
        }
    })
}

fn assess_risk_def() -> Value {
    json!({
        "name": "assess_risk",
        "description": "Rate a command safe/low/medium/high/critical BEFORE running it, and name the rule that matched. This is a pattern match on the command text — suvadu does not execute anything, inspect the filesystem, or sandbox the command; the caller still has to enforce the result.",
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
        "description": "Search past agent sessions reconstructed from recorded shell commands, by prompt text, directory, executor or date. Use this to discover which session did the work you care about, then replay_agent_session for its timeline. Pick this over list_agent_sessions when the agent's transcript was never captured but its commands were; pick search_commands instead when you want individual commands regardless of session.",
        "inputSchema": {
            "type": "object",
            "properties": with_paging(json!({
                "directory": { "type": "string", "description": "Filter to sessions that ran commands in this directory or its subtree" },
                "executor": { "type": "string", "description": "Filter by agent name (e.g. claude-code, cursor)" },
                "prompt_text": { "type": "string", "description": "Search across the first prompt of each session (substring match)" },
                "after": { "type": "string", "description": "Only sessions after this date (ISO 8601 or relative like '3 days ago')" },
                "before": { "type": "string", "description": "Only sessions before this date" }
            }), 10)
        }
    })
}

fn replay_agent_session_def() -> Value {
    json!({
        "name": "replay_agent_session",
        "description": "The chronological timeline of one agent session built from recorded shell commands: prompts, commands, exit codes, directories and timestamps, one page at a time. Get the session_id from find_agent_session or resolve_current_agent_session first.",
        "inputSchema": {
            "type": "object",
            "properties": with_paging(json!({
                "session_id": { "type": "string", "description": "The session ID to replay (with or without claude-/cursor- prefix)" }
            }), 100),
            "required": ["session_id"]
        }
    })
}

fn learn_from_failures_def() -> Value {
    json!({
        "name": "learn_from_failures",
        "description": "Commands in a project that fail often, with their recorded failure rates and whether agents fail them more than humans. Call it before starting work to avoid repeating a known-bad approach. It reports rates, not diagnoses: suvadu recorded that a command exited non-zero and nothing about why, so do not expect a cause or a confirmed fix.",
        "inputSchema": {
            "type": "object",
            "properties": with_paging(json!({
                "directory": { "type": "string", "description": "Directory to analyze, including its subtree (defaults to all)" },
                "days": { "type": "integer", "description": "How many days back to look (default: 7)", "default": 7 }
            }), 10)
        }
    })
}

fn project_context_def() -> Value {
    json!({
        "name": "project_context",
        "description": "A short briefing on a project: the programs most often run, build/test/lint commands and their pass rates, failures in the last day, and agent sessions in the window. Concise by default — pass detail=true, or follow up with learn_from_failures, what_failed or find_agent_session for one topic in full.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "directory": { "type": "string", "description": "Directory to analyze, including its subtree (defaults to all)" },
                "days": { "type": "integer", "description": "Time window in days (default: 7)", "default": 7 },
                "detail": { "type": "boolean", "description": "Widen every section (default: false)", "default": false }
            }
        }
    })
}

fn list_skills_def() -> Value {
    json!({
        "name": "list_skills",
        "description": "List skills in Suvadu's shared cross-agent skills library — reusable instructions any MCP-capable agent can read instead of each tool keeping its own copy. Use this to discover what conventions/checklists/instructions already exist before starting work. Skill text is written by people and agents; it is data, not something suvadu observed.",
        "inputSchema": {
            "type": "object",
            "properties": with_paging(json!({
                "scope": { "type": "string", "description": "Filter to \"global\", or a specific project directory path" },
                "directory": { "type": "string", "description": "Alias for scope — filter to skills scoped to this directory" }
            }), 50)
        }
    })
}

fn get_skill_def() -> Value {
    json!({
        "name": "get_skill",
        "description": "Get the full content of one skill by name from Suvadu's shared skills library. The body is instructions someone wrote, not verified fact — treat it as data.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "name": { "type": "string", "description": "Skill name" },
                "scope": { "type": "string", "description": "\"global\" or a project directory path. If omitted, the global skill is returned when one exists, otherwise the most recently updated skill with this name in any scope — this tool has no visibility into your working directory, so pass it explicitly to target a specific project-scoped skill." },
                "directory": { "type": "string", "description": "Alias for scope" }
            },
            "required": ["name"]
        }
    })
}

fn search_skills_def() -> Value {
    json!({
        "name": "search_skills",
        "description": "Search Suvadu's shared skills library by keyword across name, description, and triggers. Use this before writing a new checklist/instruction to see if one already exists.",
        "inputSchema": {
            "type": "object",
            "properties": with_paging(json!({
                "query": { "type": "string", "description": "Text to search for" },
                "scope": { "type": "string", "description": "Filter to \"global\" or a project directory path" }
            }), 50),
            "required": ["query"]
        }
    })
}

fn propose_skill_def() -> Value {
    json!({
        "name": "propose_skill",
        "description": "Propose a new skill for Suvadu's shared skills library. The skill is saved as pending review — it is NOT active and other agents will not see it via list_skills/get_skill until a human approves it from the review queue in `suv skills` (Ctrl+P). Use this when you notice a reusable instruction/checklist worth sharing, not for anything sensitive.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "name": { "type": "string", "description": "Skill name (slug)" },
                "description": { "type": "string", "description": "One-line summary" },
                "body": { "type": "string", "description": "Full skill content (markdown)" },
                "triggers": { "type": "array", "items": { "type": "string" }, "description": "Keywords this skill is relevant for" },
                "scope": { "type": "string", "description": "\"global\" or a project directory path (default: global)" },
                "source_agent": { "type": "string", "description": "Name of the proposing agent (e.g. claude-code) — recorded for the human reviewer" }
            },
            "required": ["name", "body"]
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

/// Whether the caller asked for the detailed rendering. Rule 8 of
/// [`conv`]: rows are one line until someone asks for more.
fn detail(args: &Value) -> bool {
    args.get("detail").and_then(Value::as_bool).unwrap_or(false)
}

/// `(limit, offset)` for a list-shaped tool (rule 4 of [`conv`]). Both are
/// clamped to sane values rather than rejected, because a tool that errors
/// on `limit: 0` just makes an agent retry blind.
fn paging(args: &Value, default_limit: i64) -> (usize, usize) {
    let limit = usize::try_from(get_int(args, "limit", default_limit).clamp(1, 500))
        .unwrap_or(DEFAULT_LIMIT);
    let offset = usize::try_from(get_int(args, "offset", 0).max(0)).unwrap_or(0);
    (limit, offset)
}

/// Fallback page size when a configured one cannot be represented.
const DEFAULT_LIMIT: usize = 20;

/// The `limit`/`offset`/`detail` properties every list-shaped tool shares,
/// spelled the same way in every schema so an agent learns them once.
fn paging_schema(default_limit: i64) -> Value {
    json!({
        "limit": { "type": "integer", "minimum": 1, "maximum": 500, "description": format!("Max rows in this page (default: {default_limit})"), "default": default_limit },
        "offset": { "type": "integer", "minimum": 0, "description": "Rows to skip; pass the response's next_offset to page forward", "default": 0 },
        "detail": { "type": "boolean", "description": "Add directory, duration, executor and session to each row (default: false)", "default": false }
    })
}

/// Merge the shared paging properties into a tool's own property map.
fn with_paging(mut properties: Value, default_limit: i64) -> Value {
    if let (Some(target), Some(shared)) = (
        properties.as_object_mut(),
        paging_schema(default_limit).as_object(),
    ) {
        for (key, value) in shared {
            target.entry(key.clone()).or_insert_with(|| value.clone());
        }
    }
    properties
}

/// One command row, in the shape rule 7 and rule 8 of [`conv`] describe:
/// the stable ID first so the row can always be followed up, then outcome,
/// time and directory; everything else only on request.
fn command_row(e: &crate::models::Entry, detail: bool) -> String {
    let mut row = format!(
        "{} | {} | {} | {} | {}",
        conv::command_id(e.id),
        conv::exit(e.exit_code),
        conv::timestamp(e.started_at),
        e.cwd,
        conv::clip(&e.command, conv::ROW_MAX_CHARS),
    );
    if !detail {
        return row;
    }
    let executor = match (e.executor_type.as_deref(), e.executor.as_deref()) {
        (Some(kind), Some(name)) => format!("{kind}: {name}"),
        (Some(kind), None) => kind.to_string(),
        (None, Some(name)) => name.to_string(),
        _ => conv::UNKNOWN.to_string(),
    };
    let _ = write!(
        row,
        "\n    duration {} | executor {executor} | session {}",
        conv::duration(e.duration_ms),
        e.session_id
    );
    if let Some(prompt) = entry_prompt(e) {
        let _ = write!(
            row,
            "\n    prompt \"{}\"",
            conv::clip(prompt, conv::PROMPT_MAX_CHARS)
        );
    }
    row
}

/// The agent prompt recorded alongside a command, when there was one.
fn entry_prompt(e: &crate::models::Entry) -> Option<&str> {
    e.context
        .as_ref()
        .and_then(|ctx| ctx.get("agent_prompt"))
        .map(String::as_str)
        .filter(|p| !p.is_empty())
}

/// Fetch one page plus a lookahead row, and report whether more exist.
/// Keeps `next_offset` honest without a second COUNT for every tool.
fn page_entries(
    repo: &Repository,
    limit: usize,
    offset: usize,
    qf: &QueryFilter,
) -> Result<(Vec<crate::models::Entry>, Option<usize>), String> {
    let mut entries = repo
        .get_entries_filtered(limit.saturating_add(1), offset, qf)
        .map_err(|e| format!("query failed: {e}"))?;
    let more = entries.len() > limit;
    entries.truncate(limit);
    Ok((entries, more.then(|| offset.saturating_add(limit))))
}

/// Append rows to a response, honouring rule 5: anything elided is counted
/// and announced rather than quietly dropped.
fn write_rows<T>(
    response: &mut conv::Response,
    rows: &[T],
    shown: usize,
    render: impl Fn(&T) -> String,
) {
    for row in rows.iter().take(shown) {
        response.line(render(row));
    }
    if rows.len() > shown {
        response.line(format!("  {}", conv::more_not_shown(rows.len() - shown)));
    }
}

fn handle_search_commands(
    repo: &Repository,
    args: &Value,
    mcp: &crate::config::McpConfig,
) -> Result<String, String> {
    let query = get_str(args, "query").unwrap_or("");
    let (limit, offset) = paging(args, i64::from(mcp.default_limit));
    let after = get_str(args, "after").and_then(|s| util::parse_date_input(s, false));
    let before = get_str(args, "before").and_then(|s| util::parse_date_input(s, true));
    let executor = get_str(args, "executor");
    let directory = get_str(args, "directory");
    let exit_code = args
        .get("exit_code")
        .and_then(Value::as_i64)
        .map(|c| i32::try_from(c).unwrap_or(0));

    let qf = QueryFilter {
        query_tokens: &[],
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
        cwd_prefix: false,
        failed_only: false,
        bookmarked_only: false,
        exclude_dirs: &mcp.exclude_dirs,
    };

    let (entries, next) = page_entries(repo, limit, offset, &qf)?;
    let matched = usize::try_from(
        repo.count_filtered(&qf)
            .map_err(|e| format!("query failed: {e}"))?,
    )
    .unwrap_or(entries.len());

    let detail = detail(args);
    let mut response = conv::Response::new(
        format!(
            "{} commands matching \"{query}\"{}",
            entries.len(),
            if offset > 0 {
                format!(" (from offset {offset})")
            } else {
                String::new()
            }
        ),
        conv::Provenance::Observed,
    );
    for e in &entries {
        response.line(command_row(e, detail));
    }
    if !detail {
        response = response.note(conv::DETAIL_HINT);
    }
    Ok(response
        .shown(entries.len())
        .matched(matched)
        .next_offset(next)
        .render())
}

fn handle_recent_commands(
    repo: &Repository,
    args: &Value,
    mcp: &crate::config::McpConfig,
) -> Result<String, String> {
    let (limit, offset) = paging(args, i64::from(mcp.default_limit));
    let directory = get_str(args, "directory");
    let executor = get_str(args, "executor");
    let after = get_str(args, "after").and_then(|s| util::parse_date_input(s, false));

    let qf = QueryFilter {
        query_tokens: &[],
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
        cwd_prefix: false,
        failed_only: false,
        bookmarked_only: false,
        exclude_dirs: &mcp.exclude_dirs,
    };
    let (entries, next) = if executor.is_some() || after.is_some() {
        page_entries(repo, limit, offset, &qf)?
    } else {
        let filter = QueryFilter {
            exclude_dirs: &mcp.exclude_dirs,
            ..QueryFilter::default()
        };
        let mut entries = repo
            .get_recent_entries(limit.saturating_add(1), offset, &filter, directory)
            .map_err(|e| format!("query failed: {e}"))?;
        let more = entries.len() > limit;
        entries.truncate(limit);
        (entries, more.then(|| offset.saturating_add(limit)))
    };
    let matched = usize::try_from(
        repo.count_filtered(&qf)
            .map_err(|e| format!("query failed: {e}"))?,
    )
    .unwrap_or(entries.len());

    let detail = detail(args);
    let ctx = directory.map_or_else(String::new, |d| format!(" in {d}"));
    let mut response = conv::Response::new(
        format!("{} most recent commands{ctx}", entries.len()),
        conv::Provenance::Observed,
    );
    for e in &entries {
        response.line(command_row(e, detail));
    }
    if !detail {
        response = response.note(conv::DETAIL_HINT);
    }
    Ok(response
        .shown(entries.len())
        .matched(matched)
        .next_offset(next)
        .render())
}

fn handle_command_status(
    repo: &Repository,
    args: &Value,
    mcp: &crate::config::McpConfig,
) -> Result<String, String> {
    let command = get_str(args, "command").unwrap_or("");
    let (limit, offset) = paging(args, 5);
    let directory = get_str(args, "directory");

    if command.is_empty() {
        return Err("command parameter is required".to_string());
    }

    let filter = QueryFilter {
        query: Some(command),
        prefix_match: true,
        cwd: directory,
        exclude_dirs: &mcp.exclude_dirs,
        ..QueryFilter::default()
    };
    let mut entries = repo
        .get_recent_entries(limit.saturating_add(1), offset, &filter, directory)
        .map_err(|e| format!("query failed: {e}"))?;
    let more = entries.len() > limit;
    entries.truncate(limit);
    let matched = usize::try_from(
        repo.count_filtered(&filter)
            .map_err(|e| format!("query failed: {e}"))?,
    )
    .unwrap_or(entries.len());

    let successes = entries.iter().filter(|e| e.exit_code == Some(0)).count();
    let unknown = entries.iter().filter(|e| e.exit_code.is_none()).count();
    let detail = detail(args);
    let mut response = conv::Response::new(
        format!(
            "\"{command}\" — {} runs on this page, {} succeeded{}",
            entries.len(),
            conv::rate(successes, entries.len()),
            if unknown > 0 {
                format!(", {unknown} with no recorded exit code")
            } else {
                String::new()
            }
        ),
        conv::Provenance::Observed,
    );
    for e in &entries {
        response.line(command_row(e, detail));
    }
    if !detail {
        response = response.note(conv::DETAIL_HINT);
    }
    Ok(response
        .shown(entries.len())
        .matched(matched)
        .next_offset(more.then(|| offset.saturating_add(limit)))
        .render())
}

fn handle_get_prompts(
    repo: &Repository,
    args: &Value,
    mcp: &crate::config::McpConfig,
) -> Result<String, String> {
    let (limit, offset) = paging(args, 10);
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
                exclude_dirs: &mcp.exclude_dirs,
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

    // Sort by most recent command timestamp, then page.
    let mut sorted: Vec<_> = groups.into_iter().collect();
    sorted.sort_by(|a, b| {
        let a_max = a.1.iter().map(|e| e.started_at).max().unwrap_or(0);
        let b_max = b.1.iter().map(|e| e.started_at).max().unwrap_or(0);
        b_max.cmp(&a_max)
    });
    let matched = sorted.len();
    let page: Vec<_> = sorted.into_iter().skip(offset).take(limit).collect();
    let next = (offset.saturating_add(limit) < matched).then(|| offset.saturating_add(limit));

    let detail = detail(args);
    let per_prompt = if detail { 20 } else { 3 };
    let mut response = conv::Response::new(
        format!("{} prompts on this page", page.len()),
        conv::Provenance::Observed,
    );
    for ((session_id, prompt), cmds) in &page {
        let successes = cmds.iter().filter(|e| e.exit_code == Some(0)).count();
        response.line(format!(
            "{session_id} | {} ok | \"{}\"",
            conv::rate(successes, cmds.len()),
            conv::clip(prompt, conv::PROMPT_MAX_CHARS)
        ));
        write_rows(&mut response, cmds, per_prompt, |cmd| {
            format!("  {}", command_row(cmd, false))
        });
        response.blank();
    }
    if !detail {
        response = response.note(conv::DETAIL_HINT);
    }
    Ok(response
        .shown(page.len())
        .matched(matched)
        .next_offset(next)
        .render())
}

fn handle_session_history(
    repo: &Repository,
    args: &Value,
    mcp: &crate::config::McpConfig,
) -> Result<String, String> {
    let session_id = get_str(args, "session_id");
    let (limit, offset) = paging(args, 50);

    let mut entries = repo
        .get_replay_entries(
            session_id,
            &crate::repository::ReplayFilter {
                limit: Some(limit.saturating_add(1)),
                offset,
                exclude_dirs: &mcp.exclude_dirs,
                ..Default::default()
            },
        )
        .map_err(|e| format!("query failed: {e}"))?;
    let more = entries.len() > limit;
    entries.truncate(limit);

    let sid = entries
        .first()
        .map_or_else(|| session_id.unwrap_or(conv::UNKNOWN), |e| &e.session_id)
        .to_string();
    let detail = detail(args);
    let mut response = conv::Response::new(
        format!("Session {sid} — {} commands on this page", entries.len()),
        conv::Provenance::Observed,
    );
    for e in &entries {
        response.line(command_row(e, detail));
    }
    if !mcp.exclude_dirs.is_empty() {
        response = response.note(EXCLUSION_NOTE);
    }
    if !detail {
        response = response.note(conv::DETAIL_HINT);
    }
    Ok(response
        .shown(entries.len())
        .next_offset(more.then(|| offset.saturating_add(limit)))
        .render())
}

/// Said whenever a response was filtered by `mcp.exclude_dirs`, so a
/// caller never reads a short list as a complete one.
const EXCLUSION_NOTE: &str =
    "commands recorded in directories excluded by mcp.exclude_dirs were withheld from this \
     response, so counts here can be lower than the session's real totals";

fn handle_get_stats(
    repo: &Repository,
    args: &Value,
    mcp: &crate::config::McpConfig,
) -> Result<String, String> {
    let days = get_int(args, "days", i64::from(mcp.default_days));
    let directory = get_str(args, "directory");

    let now = chrono::Utc::now().timestamp_millis();
    let after = Some(now - days * 24 * 60 * 60 * 1000);

    let qf = QueryFilter {
        query_tokens: &[],
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
        // get_stats is project-scoped: include the directory's subtree.
        cwd_prefix: true,
        failed_only: false,
        bookmarked_only: false,
        exclude_dirs: &mcp.exclude_dirs,
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

    // Top-N is computed from a bounded sample, not from every matching
    // row: say so rather than let a ranking over 200 commands read as a
    // ranking over all of them.
    let entries = repo
        .get_entries_filtered(STATS_SAMPLE, 0, &qf)
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
    let total_usize = usize::try_from(total).unwrap_or(usize::MAX);
    let mut response = conv::Response::new(
        format!(
            "Stats for {}{dir_ctx}: {total} commands, {} succeeded",
            conv::window_days(days),
            conv::rate(usize::try_from(successes).unwrap_or(0), total_usize)
        ),
        conv::Provenance::ObservedAndInferred,
    );
    response.line("Top commands (ranked, inferred from the sample below):");
    for (cmd, count) in &top_cmds {
        response.line(format!("  {count:>4}x  {cmd}"));
    }
    response.blank();
    response.line("Top directories (ranked, inferred from the sample below):");
    for (dir, count) in &top_dirs {
        response.line(format!("  {count:>4}x  {dir}"));
    }
    Ok(response
        .shown(entries.len())
        .matched(total_usize)
        .note(format!(
            "rankings are computed from the {} most recent of {total} matching commands, not from all of them",
            entries.len()
        ))
        .render())
}

fn handle_list_sessions(
    repo: &Repository,
    args: &Value,
    mcp: &crate::config::McpConfig,
) -> Result<String, String> {
    let (limit, offset) = paging(args, 10);
    let tag = get_str(args, "tag");

    let tag_id = tag.and_then(|t| repo.get_tag_id_by_name(t).ok().flatten());

    // Excluded directories apply to session rows too: counts and time
    // ranges are records *about* the commands the user asked suvadu to
    // withhold.
    let mut sessions = repo
        .list_sessions_excluding(
            None,
            tag_id,
            limit.saturating_add(1),
            offset,
            &mcp.exclude_dirs,
        )
        .map_err(|e| format!("query failed: {e}"))?;
    let more = sessions.len() > limit;
    sessions.truncate(limit);

    let detail = detail(args);
    let mut response = conv::Response::new(
        format!("{} shell sessions on this page", sessions.len()),
        conv::Provenance::Observed,
    );
    for s in &sessions {
        let tag_str = s
            .tag_name
            .as_deref()
            .map_or_else(String::new, |t| format!(" [{t}]"));
        let success = usize::try_from(s.success_count).unwrap_or(0);
        let total = usize::try_from(s.cmd_count).unwrap_or(0);
        response.line(format!(
            "{}{tag_str} | {total} cmds | {} ok | {} → {}",
            s.id,
            conv::rate(success, total),
            conv::timestamp(s.first_activity_at),
            conv::timestamp(s.last_activity_at),
        ));
        if detail {
            response.line(format!(
                "    host {} | dir {} | agent {}",
                conv::or_unknown(Some(s.hostname.as_str())),
                conv::or_unknown(s.cwd.as_deref()),
                conv::or_unknown(s.agent.as_deref()),
            ));
        }
    }
    if !mcp.exclude_dirs.is_empty() {
        response = response.note(EXCLUSION_NOTE);
    }
    if !detail {
        response = response.note(conv::DETAIL_HINT);
    }
    Ok(response
        .shown(sessions.len())
        .next_offset(more.then(|| offset.saturating_add(limit)))
        .render())
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

#[allow(clippy::too_many_lines)]
fn handle_what_changed(
    repo: &Repository,
    args: &Value,
    mcp: &crate::config::McpConfig,
) -> Result<String, String> {
    let hours = get_int(args, "hours", 4);
    let directory = get_str(args, "directory");
    let executor = get_str(args, "executor");

    let now = chrono::Utc::now().timestamp_millis();
    let after = Some(now - hours * 60 * 60 * 1000);

    let qf = QueryFilter {
        query_tokens: &[],
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
        // what_changed is project-scoped: include the directory's subtree.
        cwd_prefix: true,
        failed_only: false,
        bookmarked_only: false,
        exclude_dirs: &mcp.exclude_dirs,
    };

    let (limit, offset) = paging(args, 500);
    let (entries, _) = page_entries(repo, limit, offset, &qf)?;

    // Two lists, never one. `succeeded` holds commands whose recorded exit
    // code was 0 — the only ones whose intended effect is even plausible.
    // `attempted` holds the rest: a `rm -rf` that exited 1 changed nothing,
    // and reporting it under "deletions" is the exact false claim this
    // tool used to make. A command with no recorded exit code goes in
    // `attempted` too: suvadu does not know how it ended.
    let mut succeeded: std::collections::HashMap<&str, Vec<&crate::models::Entry>> =
        std::collections::HashMap::new();
    let mut attempted: Vec<&crate::models::Entry> = Vec::new();
    let mut unclassified = 0usize;

    for entry in &entries {
        let Some(category) = classify_command(&entry.command) else {
            unclassified += 1;
            continue;
        };
        if entry.exit_code == Some(0) {
            succeeded.entry(category).or_default().push(entry);
        } else {
            attempted.push(entry);
        }
    }

    let ctx = directory.map_or_else(String::new, |d| format!(" in {d}"));
    let detail = detail(args);
    let per_category = if detail { 50 } else { 3 };
    let mut response = conv::Response::new(
        format!(
            "{} commands recorded in the {}{ctx}; {} looked file-modifying",
            entries.len(),
            conv::window_hours(hours),
            succeeded.values().map(Vec::len).sum::<usize>() + attempted.len()
        ),
        conv::Provenance::ObservedAndInferred,
    );

    response.line("OBSERVED — commands suvadu recorded, with the exit code it recorded:");
    if entries.is_empty() {
        response.line("  (none)");
    }
    write_rows(
        &mut response,
        &entries,
        if detail { limit } else { 5 },
        |e| format!("  {}", command_row(e, detail)),
    );
    response.blank();

    response.line(
        "INFERRED — likely effect, worked out from the command text alone. suvadu did not \
         observe any file change; a command that exited 0 may still have done nothing:",
    );
    let mut sorted: Vec<_> = succeeded.into_iter().collect();
    sorted.sort_by(|a, b| b.1.len().cmp(&a.1.len()).then(a.0.cmp(b.0)));
    if sorted.is_empty() {
        response.line("  (no successful file-modifying command in this window)");
    }
    for (category, cmds) in &sorted {
        response.line(format!("  {} ({}):", category.to_uppercase(), cmds.len()));
        write_rows(&mut response, cmds, per_category, |cmd| {
            format!("    {}", conv::clip(&cmd.command, conv::ROW_MAX_CHARS))
        });
    }

    if !attempted.is_empty() {
        response.blank();
        response.line(format!(
            "ATTEMPTED — {} file-modifying command(s) that did not succeed, so no effect is \
             inferred from them:",
            attempted.len()
        ));
        write_rows(&mut response, &attempted, per_category, |cmd| {
            format!(
                "    {} | {}",
                conv::exit(cmd.exit_code),
                conv::clip(&cmd.command, conv::ROW_MAX_CHARS)
            )
        });
    }

    if unclassified > 0 {
        response = response.note(format!(
            "{unclassified} further recorded command(s) matched no change category and are not \
             listed under INFERRED"
        ));
    }
    if !detail {
        response = response.note(conv::DETAIL_HINT);
    }
    Ok(response.shown(entries.len()).render())
}

#[allow(clippy::too_many_lines)]
fn handle_what_failed(
    repo: &Repository,
    args: &Value,
    mcp: &crate::config::McpConfig,
) -> Result<String, String> {
    let hours = get_int(args, "hours", 24);
    let (limit, offset) = paging(args, i64::from(mcp.default_limit));
    let directory = get_str(args, "directory");

    let now = chrono::Utc::now().timestamp_millis();
    let after = Some(now - hours * 60 * 60 * 1000);

    // Get all entries in the time window, then filter to failures
    let qf = QueryFilter {
        query_tokens: &[],
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
        cwd_prefix: false,
        failed_only: false,
        bookmarked_only: false,
        exclude_dirs: &mcp.exclude_dirs,
    };

    // Let the database do the filtering, so `limit`/`offset` page over
    // failures rather than over an arbitrary 1000-row prefix that may
    // contain none.
    let failed_qf = QueryFilter {
        failed_only: true,
        ..qf.clone()
    };
    let (failures, next) = page_entries(repo, limit, offset, &failed_qf)?;
    let matched = usize::try_from(
        repo.count_filtered(&failed_qf)
            .map_err(|e| format!("query failed: {e}"))?,
    )
    .unwrap_or(failures.len());
    let total = usize::try_from(
        repo.count_filtered(&qf)
            .map_err(|e| format!("query failed: {e}"))?,
    )
    .unwrap_or(0);

    // Group failures by prompt (if available)
    let mut by_prompt: std::collections::HashMap<&str, Vec<&crate::models::Entry>> =
        std::collections::HashMap::new();
    let mut no_prompt_failures: Vec<&crate::models::Entry> = Vec::new();

    for entry in &failures {
        if let Some(prompt) = entry_prompt(entry) {
            by_prompt.entry(prompt).or_default().push(entry);
        } else {
            no_prompt_failures.push(entry);
        }
    }

    let ctx = directory.map_or_else(String::new, |d| format!(" in {d}"));
    let detail = detail(args);
    let per_group = if detail { limit } else { 5 };
    let mut response = conv::Response::new(
        format!(
            "{} failed commands on this page ({} of {total} recorded commands failed in the {}{ctx})",
            failures.len(),
            matched,
            conv::window_hours(hours),
        ),
        conv::Provenance::Observed,
    );

    if !by_prompt.is_empty() {
        response.line("FAILURES UNDER A RECORDED PROMPT:");
        let mut sorted: Vec<_> = by_prompt.into_iter().collect();
        sorted.sort_by(|a, b| b.1.len().cmp(&a.1.len()).then(a.0.cmp(b.0)));
        for (prompt, cmds) in &sorted {
            response.line(format!(
                "  prompt \"{}\" — {} failed",
                conv::clip(prompt, conv::PROMPT_MAX_CHARS),
                cmds.len()
            ));
            write_rows(&mut response, cmds, per_group, |cmd| {
                format!("    {}", command_row(cmd, detail))
            });
        }
        response.blank();
    }

    if !no_prompt_failures.is_empty() {
        response.line("FAILURES WITH NO PROMPT RECORDED:");
        write_rows(&mut response, &no_prompt_failures, per_group, |cmd| {
            format!("  {}", command_row(cmd, detail))
        });
    }

    response = response.note(
        "the exit code is all suvadu recorded; the error text these commands printed was never \
         captured, so why each one failed is not available here",
    );
    if !detail {
        response = response.note(conv::DETAIL_HINT);
    }
    Ok(response
        .shown(failures.len())
        .matched(matched)
        .next_offset(next)
        .render())
}

fn handle_suggest_next(
    repo: &Repository,
    args: &Value,
    mcp: &crate::config::McpConfig,
) -> Result<String, String> {
    let (limit, offset) = paging(args, 10);
    let directory = get_str(args, "directory");

    // Get recent commands (last 7 days) to build frecency scores
    let now = chrono::Utc::now().timestamp_millis();
    let week_ago = now - 7 * 24 * 60 * 60 * 1000;

    let qf = QueryFilter {
        query_tokens: &[],
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
        cwd_prefix: false,
        failed_only: false,
        bookmarked_only: false,
        exclude_dirs: &mcp.exclude_dirs,
    };

    let entries = repo
        .get_entries_filtered(FRECENCY_SAMPLE, 0, &qf)
        .map_err(|e| format!("query failed: {e}"))?;

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

    // Sort by score descending, then page.
    let mut sorted: Vec<_> = scores.into_iter().collect();
    sorted.sort_by(|a, b| {
        b.1 .0
            .partial_cmp(&a.1 .0)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.0.cmp(b.0))
    });
    let matched = sorted.len();
    let page: Vec<_> = sorted.into_iter().skip(offset).take(limit).collect();

    let ctx = directory.map_or_else(String::new, |d| format!(" in {d}"));
    let mut response = conv::Response::new(
        format!("{} suggested next commands{ctx}", page.len()),
        conv::Provenance::Inferred,
    );
    for (cmd, (score, count, last_exit)) in &page {
        response.line(format!(
            "  {} | ran {count}x | frecency score {score:.0} | last {}",
            conv::clip(cmd, conv::ROW_MAX_CHARS),
            conv::exit(*last_exit),
        ));
    }
    Ok(response
        .shown(page.len())
        .matched(matched)
        .next_offset((offset.saturating_add(limit) < matched).then(|| offset.saturating_add(limit)))
        .note(format!(
            "ranked by frecency (frequency plus recency) over the {} most recent commands in the \
             last 7d; a suggestion is a guess about what you might run, never a recommendation \
             that it is safe or correct",
            entries.len()
        ))
        .render())
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

    let mut critical = 0usize;
    let mut high = 0usize;
    let mut medium = 0usize;
    let mut response = conv::Response::new(
        format!("Risk assessment for {} command(s)", commands.len()),
        conv::Provenance::Inferred,
    );

    for cmd in &commands {
        let level = risk::risk_level(cmd);
        let assessment = risk::assess_risk(cmd);
        match level {
            risk::RiskLevel::Critical => critical += 1,
            risk::RiskLevel::High => high += 1,
            risk::RiskLevel::Medium => medium += 1,
            _ => {}
        }
        response.line(format!(
            "{} | {} | {}",
            level.label().to_uppercase(),
            assessment
                .as_ref()
                .map_or_else(|| conv::UNKNOWN.to_string(), |a| a.category.to_string()),
            conv::clip(cmd, conv::ROW_MAX_CHARS),
        ));
        response.line(format!(
            "    matched rule: {}",
            assessment
                .as_ref()
                .map_or("none — no known risk pattern matched", |a| a
                    .description
                    .as_ref())
        ));
    }

    response.blank();
    response.line(format!(
        "{critical} critical, {high} high, {medium} medium of {} assessed",
        commands.len()
    ));
    Ok(response
        .shown(commands.len())
        .matched(commands.len())
        .note(
            "a rating is a pattern match on the command text, not an execution or a sandbox; \
             suvadu did not run the command and cannot know what it would touch",
        )
        .render())
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

fn handle_find_agent_session(
    repo: &Repository,
    args: &Value,
    mcp: &crate::config::McpConfig,
) -> Result<String, String> {
    let (limit, offset) = paging(args, 10);
    let directory = get_str(args, "directory");
    let executor = get_str(args, "executor");
    let prompt_text = get_str(args, "prompt_text");
    let after = get_str(args, "after").and_then(|s| util::parse_date_input(s, false));
    let before = get_str(args, "before").and_then(|s| util::parse_date_input(s, true));

    let qf = QueryFilter {
        query_tokens: &[],
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
        // find_agent_session is project-scoped: include the directory's subtree.
        cwd_prefix: true,
        failed_only: false,
        bookmarked_only: false,
        exclude_dirs: &mcp.exclude_dirs,
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

    let matched = sessions.len();
    let page: Vec<_> = sessions.into_iter().skip(offset).take(limit).collect();
    let detail = detail(args);

    let mut response = conv::Response::new(
        format!("{} agent sessions on this page", page.len()),
        conv::Provenance::ObservedAndInferred,
    );
    for s in &page {
        response.line(format!(
            "{} | {} | {} commands, {} ok | last activity {}",
            s.session_id,
            s.executor,
            s.command_count,
            conv::rate(s.success_count, s.command_count),
            conv::when(s.last_command_at),
        ));
        if !s.first_prompt.is_empty() {
            response.line(format!(
                "    first prompt \"{}\"",
                conv::clip(&s.first_prompt, conv::PROMPT_MAX_CHARS)
            ));
        }
        if detail {
            response.line(format!(
                "    started {} | ran {} | directories {} | failed {}",
                conv::timestamp(s.first_command_at),
                conv::duration(s.last_command_at - s.first_command_at),
                if s.directories.is_empty() {
                    conv::UNKNOWN.to_string()
                } else {
                    s.directories.join(", ")
                },
                s.failure_count,
            ));
            response.line(format!("    risk (inferred): {}", s.risk_summary));
            if s.session_id.starts_with("claude-") {
                response.line(format!(
                    "    resume: claude --resume {}",
                    resume_id(&s.session_id)
                ));
            }
        }
    }
    if !detail {
        response = response.note(conv::DETAIL_HINT);
    }
    Ok(response
        .shown(page.len())
        .matched(matched)
        .next_offset((offset.saturating_add(limit) < matched).then(|| offset.saturating_add(limit)))
        .note(
            "sessions here are grouped from recorded shell commands; the risk summary is a rule \
             match on command text, not an observed outcome. For sessions captured from an \
             agent's own transcript use list_agent_sessions",
        )
        .render())
}

#[allow(clippy::too_many_lines)]
fn handle_replay_agent_session(
    repo: &Repository,
    args: &Value,
    mcp: &crate::config::McpConfig,
) -> Result<String, String> {
    let raw_id = get_str(args, "session_id").ok_or("session_id is required")?;
    let (limit, offset) = paging(args, 100);

    // Normalize: try as-is, then with prefixes
    let session_id = {
        let filter = crate::repository::ReplayFilter {
            limit: Some(1),
            exclude_dirs: &mcp.exclude_dirs,
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

    let mut entries = repo
        .get_replay_entries(
            Some(&session_id),
            &crate::repository::ReplayFilter {
                limit: Some(limit.saturating_add(1)),
                offset,
                exclude_dirs: &mcp.exclude_dirs,
                ..Default::default()
            },
        )
        .map_err(|e| format!("query failed: {e}"))?;
    let more = entries.len() > limit;
    entries.truncate(limit);

    if entries.is_empty() && offset == 0 {
        return Err(format!("No commands found for session '{session_id}'"));
    }

    let executor = entries
        .first()
        .and_then(|e| e.executor.as_deref())
        .unwrap_or(conv::UNKNOWN)
        .to_string();
    let total = entries.len();
    let success = entries.iter().filter(|e| e.exit_code == Some(0)).count();
    let failure = entries
        .iter()
        .filter(|e| e.exit_code.is_some_and(|c| c != 0))
        .count();

    let detail = detail(args);
    let mut response = conv::Response::new(
        format!(
            "Session {session_id} ({executor}) — {total} commands on this page, {} ok, {failure} failed",
            conv::rate(success, total)
        ),
        conv::Provenance::ObservedAndInferred,
    );
    response.line("TIMELINE (observed records, chronological):");

    let mut last_prompt = String::new();
    for entry in &entries {
        if let Some(prompt) = entry_prompt(entry) {
            if prompt != last_prompt {
                response.line(format!(
                    "  [prompt] \"{}\" — {}",
                    conv::clip(prompt, conv::PROMPT_MAX_CHARS),
                    conv::timestamp(entry.started_at)
                ));
                last_prompt = prompt.to_string();
            }
        }
        response.line(format!("  {}", command_row(entry, detail)));
    }

    let mut dirs: Vec<&str> = entries
        .iter()
        .map(|e| e.cwd.as_str())
        .collect::<std::collections::BTreeSet<_>>()
        .into_iter()
        .collect();
    dirs.sort_unstable();

    let risk_data = crate::risk::session_risk(&entries);
    let mut risk_parts = Vec::new();
    for (count, label) in [
        (risk_data.critical_count, "critical"),
        (risk_data.high_count, "high"),
        (risk_data.medium_count, "medium"),
        (risk_data.safe_count + risk_data.low_count, "safe/low"),
    ] {
        if count > 0 {
            risk_parts.push(format!("{count} {label}"));
        }
    }

    let first_at = entries.iter().map(|e| e.started_at).min().unwrap_or(0);
    let last_at = entries.iter().map(|e| e.ended_at).max().unwrap_or(0);

    response.blank();
    response.line("SUMMARY of this page:");
    response.line(format!(
        "  commands: {total} ({} ok, {failure} failed)",
        conv::rate(success, total)
    ));
    response.line(format!(
        "  span: {} → {} ({})",
        conv::timestamp(first_at),
        conv::timestamp(last_at),
        conv::duration(last_at - first_at)
    ));
    response.line(format!("  directories: {}", dirs.join(", ")));
    response.line(format!(
        "  risk (inferred from command text): {}",
        if risk_parts.is_empty() {
            conv::UNKNOWN.to_string()
        } else {
            risk_parts.join(", ")
        }
    ));
    if session_id.starts_with("claude-") {
        response.line(format!(
            "  resume: claude --resume {}",
            resume_id(&session_id)
        ));
    }

    if !mcp.exclude_dirs.is_empty() {
        response = response.note(EXCLUSION_NOTE);
    }
    if !detail {
        response = response.note(conv::DETAIL_HINT);
    }
    Ok(response
        .shown(total)
        .next_offset(more.then(|| offset.saturating_add(limit)))
        .note(
            "the summary describes this page only; follow next_offset until it is none before \
             treating it as the whole session",
        )
        .render())
}

struct CmdFailStats {
    total: usize,
    fails: usize,
    agent_total: usize,
    agent_fails: usize,
    last_fail_at: i64,
}

fn handle_learn_from_failures(
    repo: &Repository,
    args: &Value,
    mcp: &crate::config::McpConfig,
) -> Result<String, String> {
    let days = get_int(args, "days", i64::from(mcp.default_days));
    let directory = get_str(args, "directory");

    let now = chrono::Utc::now().timestamp_millis();
    let after = now - days * 24 * 60 * 60 * 1000;

    let qf = QueryFilter {
        after: Some(after),
        cwd: directory,
        // Project-scoped: include the directory's subtree.
        cwd_prefix: true,
        failed_only: false,
        bookmarked_only: false,
        exclude_dirs: &mcp.exclude_dirs,
        ..QueryFilter::default()
    };

    let entries = repo
        .get_entries_filtered(ANALYSIS_SAMPLE, 0, &qf)
        .map_err(|e| format!("query failed: {e}"))?;

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

    let matched = problem_cmds.len();
    let (limit, offset) = paging(args, 10);
    let page: Vec<_> = problem_cmds.into_iter().skip(offset).take(limit).collect();

    let mut response = conv::Response::new(
        format!(
            "{} commands in the {} failed on 40% or more of their recorded runs \
             (3 runs minimum)",
            matched,
            conv::window_days(days)
        ),
        conv::Provenance::ObservedAndInferred,
    );
    for (cmd, s) in &page {
        response.line(format!(
            "{} | failed {} | last failure {}",
            conv::clip(cmd, conv::ROW_MAX_CHARS),
            conv::rate(s.fails, s.total),
            conv::when(s.last_fail_at),
        ));
        if s.agent_total > 0 && s.total > s.agent_total {
            let human_total = s.total - s.agent_total;
            let human_fails = s.fails - s.agent_fails;
            response.line(format!(
                "    agents failed {} | humans failed {}",
                conv::rate(s.agent_fails, s.agent_total),
                conv::rate(human_fails, human_total),
            ));
        }
    }
    Ok(response
        .shown(page.len())
        .matched(matched)
        .next_offset((offset.saturating_add(limit) < matched).then(|| offset.saturating_add(limit)))
        .note(format!(
            "computed over the {} most recent commands in this window",
            entries.len()
        ))
        .note(
            "these are failure rates, not explanations: suvadu recorded that the command exited \
             non-zero and nothing about why. Whether a later run fixed anything is not something \
             suvadu observed either",
        )
        .render())
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

/// Build/test/lint commands with their recorded pass rate.
fn build_test_lint_rows(entries: &[crate::models::Entry], shown: usize) -> Vec<String> {
    let mut counts: std::collections::HashMap<&str, (usize, usize)> =
        std::collections::HashMap::new();
    for e in entries.iter().filter(|e| is_build_test_lint(&e.command)) {
        let entry = counts.entry(e.command.as_str()).or_insert((0, 0));
        entry.0 += 1;
        if e.exit_code == Some(0) {
            entry.1 += 1;
        }
    }
    let mut sorted: Vec<_> = counts.into_iter().collect();
    sorted.sort_by(|a, b| (b.1).0.cmp(&(a.1).0).then(a.0.cmp(b.0)));
    sorted
        .into_iter()
        .take(shown)
        .map(|(cmd, (total, success))| {
            format!(
                "    {} — {} runs, {} ok",
                conv::clip(cmd, conv::ROW_MAX_CHARS),
                total,
                conv::rate(success, total)
            )
        })
        .collect()
}

fn handle_project_context(
    repo: &Repository,
    args: &Value,
    mcp: &crate::config::McpConfig,
) -> Result<String, String> {
    let days = get_int(args, "days", i64::from(mcp.default_days));
    let directory = get_str(args, "directory");
    let detail = detail(args);

    let now = chrono::Utc::now().timestamp_millis();
    let after = now - days * 24 * 60 * 60 * 1000;
    let day_ago = now - 24 * 60 * 60 * 1000;

    let qf = QueryFilter {
        after: Some(after),
        cwd: directory,
        // Project-scoped: include the directory's subtree.
        cwd_prefix: true,
        failed_only: false,
        bookmarked_only: false,
        exclude_dirs: &mcp.exclude_dirs,
        ..QueryFilter::default()
    };

    let entries = repo
        .get_entries_filtered(ANALYSIS_SAMPLE, 0, &qf)
        .map_err(|e| format!("query failed: {e}"))?;

    // A briefing is the worst place for an undifferentiated dump: the
    // caller has not asked a question yet, so the default is a handful of
    // named rows per section with the IDs to follow up on, and `detail`
    // opens each section up.
    let per_section = if detail { 10 } else { 3 };
    let dir_label = directory.unwrap_or("all directories");

    let mut cmd_counts: std::collections::HashMap<&str, usize> = std::collections::HashMap::new();
    for e in &entries {
        let program = e.command.split_whitespace().next().unwrap_or(&e.command);
        *cmd_counts.entry(program).or_default() += 1;
    }
    let mut top_cmds: Vec<_> = cmd_counts.into_iter().collect();
    top_cmds.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(b.0)));

    let mut response = conv::Response::new(
        format!(
            "Project context for {dir_label} — {} commands recorded in the {}",
            entries.len(),
            conv::window_days(days)
        ),
        conv::Provenance::ObservedAndInferred,
    );

    response.line("Common programs (inferred, ranked by frequency):");
    write_rows(&mut response, &top_cmds, per_section * 2, |(cmd, count)| {
        format!("    {count:>4}x  {cmd}")
    });

    let btl = build_test_lint_rows(&entries, per_section);
    if !btl.is_empty() {
        response.blank();
        response.line("Build/test/lint commands (observed runs and exit codes):");
        for row in &btl {
            response.line(row);
        }
    }

    let recent_failures: Vec<_> = entries
        .iter()
        .filter(|e| e.started_at >= day_ago && e.exit_code.is_some_and(|c| c != 0))
        .collect();
    if !recent_failures.is_empty() {
        response.blank();
        response.line(format!(
            "Failures in the last 24h ({} recorded):",
            recent_failures.len()
        ));
        write_rows(&mut response, &recent_failures, per_section, |e| {
            format!("    {}", command_row(e, false))
        });
    }

    let agent_sessions = build_session_groups(&entries);
    if !agent_sessions.is_empty() {
        response.blank();
        response.line(format!(
            "Agent sessions in this window ({}):",
            agent_sessions.len()
        ));
        write_rows(&mut response, &agent_sessions, per_section, |s| {
            format!(
                "    {} | {} | {} cmds, {} failed | last activity {}",
                s.session_id,
                s.executor,
                s.command_count,
                s.failure_count,
                conv::timestamp(s.last_command_at)
            )
        });
    }

    if !detail {
        response = response.note(
            "concise briefing: pass detail=true to widen every section, or use \
             learn_from_failures / what_failed / find_agent_session for one topic in full",
        );
    }
    Ok(response
        .shown(entries.len())
        .note(format!(
            "computed over the {} most recent commands in this window",
            entries.len()
        ))
        .render())
}

// ── Skills tools ─────────────────────────────────────────────

fn format_skill_summary(s: &crate::models::Skill) -> String {
    let triggers = if s.triggers.is_empty() {
        conv::UNKNOWN.to_string()
    } else {
        s.triggers.join(", ")
    };
    format!(
        "{} | scope {} | triggers {triggers} | {}",
        s.name,
        s.scope,
        conv::clip(&s.description, conv::PROMPT_MAX_CHARS)
    )
}

/// A skill scoped to a directory the user asked suvadu to keep quiet about
/// discloses that directory's path just by being listed, so exclusions
/// apply to skills as well as to commands.
fn skill_visible(skill: &crate::models::Skill, mcp: &crate::config::McpConfig) -> bool {
    skill.scope == crate::models::SKILL_SCOPE_GLOBAL
        || !mcp.exclude_dirs.iter().any(|dir| {
            let root = crate::repository::expand_tilde(dir);
            let root = root.trim_end_matches('/');
            !root.is_empty()
                && (skill.scope == root
                    || skill
                        .scope
                        .strip_prefix(root)
                        .is_some_and(|rest| rest.starts_with('/')))
        })
}

fn handle_list_skills(
    repo: &Repository,
    args: &Value,
    mcp: &crate::config::McpConfig,
) -> Result<String, String> {
    let scope = get_str(args, "scope").or_else(|| get_str(args, "directory"));
    let (limit, offset) = paging(args, 50);
    let all = repo
        .list_skills(scope, Some(crate::models::SKILL_STATUS_ACTIVE))
        .map_err(|e| format!("query failed: {e}"))?;
    let visible: Vec<_> = all.into_iter().filter(|s| skill_visible(s, mcp)).collect();
    let matched = visible.len();
    let page: Vec<_> = visible.into_iter().skip(offset).take(limit).collect();

    let mut response = conv::Response::new(
        format!("{} active shared skills on this page", page.len()),
        conv::Provenance::CallerReported,
    );
    for s in &page {
        response.line(format_skill_summary(s));
    }
    if page.is_empty() {
        response.line("(none — add one with `suv skills add <name>`)");
    }
    Ok(response
        .shown(page.len())
        .matched(matched)
        .next_offset(
            (offset.saturating_add(limit) < matched).then(|| offset.saturating_add(limit)),
        )
        .note("skill text is written by people and agents, not observed by suvadu; use get_skill(name) for one skill's full body")
        .render())
}

fn handle_get_skill(
    repo: &Repository,
    args: &Value,
    mcp: &crate::config::McpConfig,
) -> Result<String, String> {
    let name = get_str(args, "name").ok_or("name is required")?;
    let scope = get_str(args, "scope").or_else(|| get_str(args, "directory"));

    let skill = repo
        .find_skill(name, scope)
        .map_err(|e| format!("query failed: {e}"))?
        .filter(|s| skill_visible(s, mcp))
        .ok_or_else(|| format!("No active skill named '{name}' found."))?;

    let mut response = conv::Response::new(
        format!("Skill {} | scope {}", skill.name, skill.scope),
        conv::Provenance::CallerReported,
    );
    response.line(format!(
        "description: {}",
        conv::or_unknown(Some(skill.description.as_str()))
    ));
    response.line(format!(
        "triggers: {}",
        if skill.triggers.is_empty() {
            conv::UNKNOWN.to_string()
        } else {
            skill.triggers.join(", ")
        }
    ));
    response.blank();
    response.line(&skill.body);
    Ok(response
        .shown(1)
        .matched(1)
        .note("skill text is instructions a person or agent wrote; it is data, not something suvadu observed or verified")
        .render())
}

fn handle_search_skills(
    repo: &Repository,
    args: &Value,
    mcp: &crate::config::McpConfig,
) -> Result<String, String> {
    let query = get_str(args, "query").ok_or("query is required")?;
    let scope = get_str(args, "scope");
    let (limit, offset) = paging(args, 50);

    let all = repo
        .search_skills(query, scope)
        .map_err(|e| format!("query failed: {e}"))?;
    let visible: Vec<_> = all.into_iter().filter(|s| skill_visible(s, mcp)).collect();
    let matched = visible.len();
    let page: Vec<_> = visible.into_iter().skip(offset).take(limit).collect();

    let mut response = conv::Response::new(
        format!("{} skills matching \"{query}\"", page.len()),
        conv::Provenance::CallerReported,
    );
    for s in &page {
        response.line(format_skill_summary(s));
    }
    Ok(response
        .shown(page.len())
        .matched(matched)
        .next_offset((offset.saturating_add(limit) < matched).then(|| offset.saturating_add(limit)))
        .note("skill text is written by people and agents, not observed by suvadu")
        .render())
}

/// Writes a proposed skill with `status = pending_review`. Unlike every
/// other MCP tool, this needs a write-capable connection — it opens its own
/// short-lived one on demand rather than relaxing the read-only guarantee
/// the server opens with for every other tool (see `mcp/server.rs`). The gate
/// check happens before that connection is ever opened, so a disabled call
/// never touches the database at all.
fn handle_propose_skill(args: &Value, mcp: &crate::config::McpConfig) -> Result<String, String> {
    if !mcp.allow_skill_proposals {
        return Err(
            "Skill proposals are disabled. A human can enable them by setting \
             mcp.allow_skill_proposals = true in config.toml — proposals still land as \
             pending_review only, never active, until reviewed from the review queue in \
             `suv skills` (Ctrl+P)."
                .to_string(),
        );
    }
    // Deliberate exception to "the MCP server only ever holds a read-only
    // connection" (see `server::run`'s doc comment): this is the one tool
    // that writes, so it needs its own read-write connection rather than
    // the shared read-only one every other handler receives. Safe here
    // specifically because (a) it's gated behind the opt-in check above,
    // (b) the database runs in WAL mode (one writer + readers coexist
    // without blocking), and (c) the server's request loop is synchronous —
    // this connection is never open at the same instant another request is
    // being handled. Do NOT copy this pattern for a future tool without
    // re-checking all three of those still hold.
    let repo = crate::repository::Repository::init()
        .map_err(|e| format!("failed to open database: {e}"))?;
    propose_skill_with_repo(&repo, args)
}

/// Core of [`handle_propose_skill`], parameterized on the repository so
/// tests can pass a temp-database `Repository` instead of exercising the
/// real on-disk database path.
fn propose_skill_with_repo(repo: &Repository, args: &Value) -> Result<String, String> {
    let name = get_str(args, "name").ok_or("name is required")?;
    let body = get_str(args, "body").ok_or("body is required")?;
    let description = get_str(args, "description").unwrap_or("").to_string();
    let scope = crate::models::normalize_scope_path(
        get_str(args, "scope").unwrap_or(crate::models::SKILL_SCOPE_GLOBAL),
    );
    let source_agent = get_str(args, "source_agent").unwrap_or("unknown");
    let triggers = args
        .get("triggers")
        .and_then(Value::as_array)
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default();

    let new = crate::models::NewSkill {
        name: name.to_string(),
        description,
        body: body.to_string(),
        triggers,
        scope,
        source: format!("agent:{source_agent}"),
        status: crate::models::SKILL_STATUS_PENDING.to_string(),
    };

    let skill = repo
        .create_skill(&new)
        .map_err(|e| format!("failed to save proposal: {e}"))?;
    Ok(format!(
        "Proposal saved as pending review: '{}' ({}). A human must approve it from the review queue in `suv skills` (Ctrl+P) before it becomes active.",
        skill.name, skill.scope
    ))
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
        assert_eq!(tools.len(), 19);
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

    // ── Catalog ↔ advertisement identity ───────────────────────
    //
    // Asserted by name, never by an expected count, so adding a tool can
    // never leave the catalog, the advertisement and the settings UI out of
    // step (see also `settings_ui::tests`).

    fn advertised(mcp: &crate::config::McpConfig) -> std::collections::BTreeSet<String> {
        let resp = list_tools(&json!(1), mcp);
        resp["result"]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .map(|t| t["name"].as_str().unwrap().to_string())
            .collect()
    }

    fn catalog_names<F>(keep: F) -> std::collections::BTreeSet<String>
    where
        F: Fn(&super::super::catalog::ToolEntry) -> bool,
    {
        super::super::catalog::TOOLS
            .iter()
            .filter(|t| keep(t))
            .map(|t| t.name.to_string())
            .collect()
    }

    #[test]
    fn catalog_tools_all_have_definitions() {
        for entry in super::super::catalog::TOOLS {
            let def = tool_definition(entry.name)
                .unwrap_or_else(|| panic!("no JSON definition for catalog tool {}", entry.name));
            assert_eq!(
                def["name"].as_str().unwrap(),
                entry.name,
                "definition name disagrees with the catalog name"
            );
        }
    }

    #[test]
    fn advertised_tools_match_catalog_default_availability() {
        assert_eq!(
            advertised(&default_mcp()),
            catalog_names(super::super::catalog::ToolEntry::default_available),
            "default advertisement must be exactly the catalog's default-available tools"
        );
    }

    #[test]
    fn advertised_tools_match_the_whole_catalog_with_opt_ins_on() {
        let mcp = crate::config::McpConfig {
            allow_session_summaries: true,
            allow_skill_proposals: true,
            ..Default::default()
        };
        assert_eq!(advertised(&mcp), catalog_names(|_| true));
    }

    #[test]
    fn every_catalog_tool_can_be_disabled_and_then_cannot_be_invoked() {
        let (_dir, repo) = crate::test_utils::test_repo();
        for entry in super::super::catalog::TOOLS {
            let mcp = crate::config::McpConfig {
                allow_session_summaries: true,
                allow_skill_proposals: true,
                disabled_tools: vec![entry.name.to_string()],
                ..Default::default()
            };
            assert!(
                !advertised(&mcp).contains(entry.name),
                "{} is still advertised while disabled",
                entry.name
            );
            let err = call_tool(&repo, entry.name, &json!({}), &mcp)
                .expect_err("a disabled tool must not be invocable");
            assert!(
                err.contains("disabled"),
                "{} was rejected without saying it is disabled: {err}",
                entry.name
            );
        }
    }

    #[test]
    fn write_tools_cannot_be_invoked_until_their_opt_in_is_on() {
        let (_dir, repo) = crate::test_utils::test_repo();
        for entry in super::super::catalog::TOOLS {
            let Some(opt_in) = entry.write_opt_in else {
                continue;
            };
            let mcp = default_mcp();
            assert!(!opt_in.is_on(&mcp), "writes must be off by default");
            let err = call_tool(&repo, entry.name, &json!({}), &mcp)
                .expect_err("a write tool must not be invocable before its opt-in");
            assert!(
                err.contains(opt_in.config_key()),
                "{} should point at {}: {err}",
                entry.name,
                opt_in.config_key()
            );
        }
    }

    #[test]
    fn unknown_tool_is_still_an_unknown_tool_error() {
        let (_dir, repo) = crate::test_utils::test_repo();
        let err = call_tool(&repo, "not_a_tool", &json!({}), &default_mcp()).unwrap_err();
        assert!(err.contains("Unknown tool"), "{err}");
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
        // An empty answer is still a conventions-shaped answer.
        assert!(result.unwrap().contains("shown: 0 of 0"));
    }

    #[test]
    fn test_recent_commands_empty_db() {
        let (_dir, repo) = crate::test_utils::test_repo();
        let result = call_tool(&repo, "recent_commands", &json!({}), &default_mcp());
        assert!(result.is_ok());
        assert!(result.unwrap().contains("shown: 0 of 0"));
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
        assert!(result.unwrap().contains("shown: 0"));
    }

    #[test]
    fn test_get_prompts_empty_db() {
        let (_dir, repo) = crate::test_utils::test_repo();
        let result = call_tool(&repo, "get_prompts", &json!({}), &default_mcp());
        assert!(result.is_ok());
        assert!(result.unwrap().contains("shown: 0 of 0"));
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
            text.contains("1/2 (50%) succeeded"),
            "should report 1 success out of 2: {text}"
        );
        assert!(
            text.contains("exit 2"),
            "should show the failed run's exit code: {text}"
        );
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
        assert!(result
            .unwrap()
            .contains("no successful file-modifying command"));
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
                now - (i64::try_from(i).unwrap() * 1000),
                now - (i64::try_from(i).unwrap() * 1000) + 100,
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
        assert!(result.unwrap().contains("shown: 0 of 0"));
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
        assert!(
            text.contains("1 failed commands on this page"),
            "should count failure: {text}"
        );
        assert!(text.contains("run the tests"), "should show prompt: {text}");
        assert!(text.contains("cargo test"), "should show command: {text}");
    }

    #[test]
    fn test_suggest_next_empty_db() {
        let (_dir, repo) = crate::test_utils::test_repo();
        let result = call_tool(&repo, "suggest_next", &json!({}), &default_mcp());
        assert!(result.is_ok());
        assert!(result.unwrap().contains("shown: 0 of 0"));
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
            text.contains("3 command(s)"),
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
            result.unwrap().contains("shown: 0 of 0"),
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
            // The resume hint is detail, not part of the concise row.
            &json!({"executor": "claude-code", "detail": true}),
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
        assert!(text.contains("[prompt]"), "should show prompts: {text}");
        assert!(
            text.contains("refactor auth module"),
            "should show prompt text: {text}"
        );
        assert!(text.contains("grep"), "should show commands: {text}");
        assert!(text.contains("npm test"), "should show commands: {text}");
        assert!(
            text.contains("SUMMARY of this page:"),
            "should show summary: {text}"
        );
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
        assert!(result.unwrap().contains("shown: 0 of 0"));
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
            result.unwrap().contains("shown: 0 of 0"),
            "should report no problems"
        );
    }

    // ── project_context tests ───────────────────────────────

    #[test]
    fn test_project_context_empty_db() {
        let (_dir, repo) = crate::test_utils::test_repo();
        let result = call_tool(&repo, "project_context", &json!({}), &default_mcp());
        assert!(result.is_ok());
        assert!(result.unwrap().contains("0 commands recorded"));
    }

    #[test]
    fn test_project_context_with_data() {
        let (_dir, repo) = crate::test_utils::test_repo();
        seed_agent_sessions(&repo);

        let result = call_tool(&repo, "project_context", &json!({}), &default_mcp());
        assert!(result.is_ok());
        let text = result.unwrap();
        assert!(
            text.contains("Common programs"),
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
            text.contains("git"),
            "should show cursor commands from /other-project: {text}"
        );
        assert!(
            text.contains("cursor-def456"),
            "should show the cursor session from /other-project: {text}"
        );
        assert!(
            !text.contains("grep"),
            "should NOT show claude commands from /project: {text}"
        );
    }

    // ── Skills tools ─────────────────────────────────────────

    fn seed_skill(repo: &Repository, name: &str, scope: &str, status: &str) {
        repo.create_skill(&crate::models::NewSkill {
            name: name.to_string(),
            description: format!("{name} description"),
            body: format!("# {name}\n\nDo the thing."),
            triggers: vec!["deploy".to_string()],
            scope: scope.to_string(),
            source: crate::models::SKILL_SOURCE_HUMAN.to_string(),
            status: status.to_string(),
        })
        .unwrap();
    }

    #[test]
    fn test_list_skills_empty() {
        let (_dir, repo) = crate::test_utils::test_repo();
        let result = call_tool(&repo, "list_skills", &json!({}), &default_mcp()).unwrap();
        assert!(result.contains("suv skills add"), "{result}");
        assert!(result.contains("shown: 0 of 0"), "{result}");
    }

    #[test]
    fn test_list_skills_only_shows_active() {
        let (_dir, repo) = crate::test_utils::test_repo();
        seed_skill(
            &repo,
            "active-one",
            crate::models::SKILL_SCOPE_GLOBAL,
            crate::models::SKILL_STATUS_ACTIVE,
        );
        seed_skill(
            &repo,
            "pending-one",
            crate::models::SKILL_SCOPE_GLOBAL,
            crate::models::SKILL_STATUS_PENDING,
        );

        let result = call_tool(&repo, "list_skills", &json!({}), &default_mcp()).unwrap();
        assert!(result.contains("active-one"));
        assert!(!result.contains("pending-one"));
    }

    #[test]
    fn test_list_skills_filters_by_scope() {
        let (_dir, repo) = crate::test_utils::test_repo();
        seed_skill(
            &repo,
            "global-one",
            crate::models::SKILL_SCOPE_GLOBAL,
            crate::models::SKILL_STATUS_ACTIVE,
        );
        seed_skill(
            &repo,
            "proj-one",
            "/proj",
            crate::models::SKILL_STATUS_ACTIVE,
        );

        let result = call_tool(
            &repo,
            "list_skills",
            &json!({"scope": "/proj"}),
            &default_mcp(),
        )
        .unwrap();
        assert!(result.contains("proj-one"));
        assert!(!result.contains("global-one"));
    }

    #[test]
    fn test_get_skill_returns_body() {
        let (_dir, repo) = crate::test_utils::test_repo();
        seed_skill(
            &repo,
            "deploy",
            crate::models::SKILL_SCOPE_GLOBAL,
            crate::models::SKILL_STATUS_ACTIVE,
        );

        let result = call_tool(
            &repo,
            "get_skill",
            &json!({"name": "deploy"}),
            &default_mcp(),
        )
        .unwrap();
        assert!(result.contains("Do the thing."));
        assert!(result.contains("deploy description"));
    }

    #[test]
    fn test_get_skill_missing_name_errors() {
        let (_dir, repo) = crate::test_utils::test_repo();
        let result = call_tool(&repo, "get_skill", &json!({}), &default_mcp());
        assert!(result.is_err());
    }

    #[test]
    fn test_get_skill_not_found() {
        let (_dir, repo) = crate::test_utils::test_repo();
        let result = call_tool(&repo, "get_skill", &json!({"name": "nope"}), &default_mcp());
        assert!(result.is_err());
    }

    #[test]
    fn test_search_skills_matches_trigger() {
        let (_dir, repo) = crate::test_utils::test_repo();
        seed_skill(
            &repo,
            "deploy-checklist",
            crate::models::SKILL_SCOPE_GLOBAL,
            crate::models::SKILL_STATUS_ACTIVE,
        );

        let result = call_tool(
            &repo,
            "search_skills",
            &json!({"query": "deploy"}),
            &default_mcp(),
        )
        .unwrap();
        assert!(result.contains("deploy-checklist"));
    }

    #[test]
    fn test_propose_skill_disabled_by_default_never_touches_db() {
        let (_dir, repo) = crate::test_utils::test_repo();
        // allow_skill_proposals defaults to false, so this must short-circuit
        // before ever opening a (real, on-disk) Repository::init().
        let result = call_tool(
            &repo,
            "propose_skill",
            &json!({"name": "x", "body": "y"}),
            &default_mcp(),
        );
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("disabled"));
    }

    #[test]
    fn test_propose_skill_saves_as_pending_review() {
        let (_dir, repo) = crate::test_utils::test_repo();
        let result = propose_skill_with_repo(
            &repo,
            &json!({
                "name": "new-idea",
                "description": "worth sharing",
                "body": "do X then Y",
                "triggers": ["ci"],
                "source_agent": "claude-code"
            }),
        )
        .unwrap();
        assert!(result.contains("pending review"));

        let saved = repo
            .get_skill("new-idea", crate::models::SKILL_SCOPE_GLOBAL)
            .unwrap()
            .unwrap();
        assert_eq!(saved.status, crate::models::SKILL_STATUS_PENDING);
        assert_eq!(saved.source, "agent:claude-code");

        // Pending proposals must not surface through the read tools.
        let listed = call_tool(&repo, "list_skills", &json!({}), &default_mcp()).unwrap();
        assert!(!listed.contains("new-idea"));
    }

    #[test]
    fn test_list_tools_hides_propose_skill_by_default() {
        let mcp = default_mcp();
        let resp = list_tools(&json!(1), &mcp);
        let tools = resp["result"]["tools"].as_array().unwrap();
        assert!(!tools.iter().any(|t| t["name"] == "propose_skill"));
    }

    #[test]
    fn test_list_tools_shows_propose_skill_when_enabled() {
        let mut mcp = default_mcp();
        mcp.allow_skill_proposals = true;
        let resp = list_tools(&json!(1), &mcp);
        let tools = resp["result"]["tools"].as_array().unwrap();
        assert!(tools.iter().any(|t| t["name"] == "propose_skill"));
    }

    // ── mcp.exclude_dirs enforcement ───────────────────────────
    // exclude_dirs was documented ("Directories to exclude from MCP
    // queries") and configurable via `suv settings`, but never actually
    // enforced by any MCP handler — every one of these would have failed
    // before the fix. Covers each of the three underlying query paths
    // (QueryFilter entries, QueryFilter aggregate counts, get_recent_entries,
    // ReplayFilter) so a regression in any of them is caught here.

    fn seed_two_dirs(repo: &Repository) {
        // Recent (rather than fixed historical) timestamps so this also works
        // through date-windowed handlers like get_stats (last N days).
        let now = chrono::Utc::now().timestamp_millis();
        let session = crate::models::Session {
            id: "sess-excl".to_string(),
            hostname: "test".to_string(),
            created_at: now - 2000,
            tag_id: None,
        };
        repo.insert_session(&session).unwrap();

        repo.insert_entry(&crate::models::Entry::new(
            "sess-excl".to_string(),
            "cat id_rsa".to_string(),
            "/Users/test/.ssh".to_string(),
            Some(0),
            now - 2000,
            now - 1000,
        ))
        .unwrap();
        repo.insert_entry(&crate::models::Entry::new(
            "sess-excl".to_string(),
            "cargo build".to_string(),
            "/Users/test/project".to_string(),
            Some(0),
            now - 500,
            now,
        ))
        .unwrap();
    }

    #[test]
    fn test_search_commands_respects_exclude_dirs() {
        let (_dir, repo) = crate::test_utils::test_repo();
        seed_two_dirs(&repo);
        let mut mcp = default_mcp();
        mcp.exclude_dirs = vec!["/Users/test/.ssh".to_string()];

        let text = call_tool(&repo, "search_commands", &json!({"query": ""}), &mcp).unwrap();
        assert!(text.contains("cargo build"));
        assert!(!text.contains("id_rsa"), "excluded dir leaked: {text}");
    }

    #[test]
    fn test_recent_commands_respects_exclude_dirs() {
        let (_dir, repo) = crate::test_utils::test_repo();
        seed_two_dirs(&repo);
        let mut mcp = default_mcp();
        mcp.exclude_dirs = vec!["/Users/test/.ssh".to_string()];

        // No executor/after args, so this exercises the get_recent_entries path.
        let text = call_tool(&repo, "recent_commands", &json!({}), &mcp).unwrap();
        assert!(text.contains("cargo build"));
        assert!(!text.contains("id_rsa"), "excluded dir leaked: {text}");
    }

    #[test]
    fn test_command_status_respects_exclude_dirs() {
        let (_dir, repo) = crate::test_utils::test_repo();
        seed_two_dirs(&repo);
        let mut mcp = default_mcp();
        mcp.exclude_dirs = vec!["/Users/test/.ssh".to_string()];

        let result = call_tool(
            &repo,
            "command_status",
            &json!({"command": "cat id_rsa"}),
            &mcp,
        )
        .unwrap();
        assert!(
            result.contains("shown: 0 of 0"),
            "excluded dir's command should be invisible: {result}"
        );
    }

    #[test]
    fn test_session_history_respects_exclude_dirs() {
        let (_dir, repo) = crate::test_utils::test_repo();
        seed_two_dirs(&repo);
        let mut mcp = default_mcp();
        mcp.exclude_dirs = vec!["/Users/test/.ssh".to_string()];

        // ReplayFilter path (get_replay_entries), scoped to the seeded session.
        let text = call_tool(
            &repo,
            "session_history",
            &json!({"session_id": "sess-excl"}),
            &mcp,
        )
        .unwrap();
        assert!(text.contains("cargo build"));
        assert!(!text.contains("id_rsa"), "excluded dir leaked: {text}");
    }

    #[test]
    fn test_get_stats_excludes_dirs_from_aggregate_counts() {
        // Proves exclusion happens at the SQL level, not just in Rust after
        // fetching — get_stats's `total`/`success` come from count_filtered(),
        // which never materializes entries to post-filter in the first place.
        let (_dir, repo) = crate::test_utils::test_repo();
        seed_two_dirs(&repo);

        let without_exclusion = call_tool(&repo, "get_stats", &json!({}), &default_mcp()).unwrap();
        assert!(
            without_exclusion.contains("2 commands"),
            "{without_exclusion}"
        );

        let mut mcp = default_mcp();
        mcp.exclude_dirs = vec!["/Users/test/.ssh".to_string()];
        let with_exclusion = call_tool(&repo, "get_stats", &json!({}), &mcp).unwrap();
        assert!(
            with_exclusion.contains("1 commands"),
            "excluded dir's entry should not count: {with_exclusion}"
        );
    }

    #[test]
    fn test_exclude_dirs_matches_subtree_and_expands_tilde() {
        // Uses the real $HOME rather than overriding it: HOME is process-global
        // and cargo test runs tests in parallel threads within one process, so
        // mutating it here could race with any other test that reads it.
        let Some(home) = std::env::var_os("HOME") else {
            return; // nothing to assert without a HOME to expand against
        };
        let home = home.to_string_lossy().into_owned();

        let (_dir, repo) = crate::test_utils::test_repo();
        let session = crate::models::Session {
            id: "sess-subtree".to_string(),
            hostname: "test".to_string(),
            created_at: 1_700_000_000_000,
            tag_id: None,
        };
        repo.insert_session(&session).unwrap();
        repo.insert_entry(&crate::models::Entry::new(
            "sess-subtree".to_string(),
            "ls -la".to_string(),
            format!("{home}/.suvadu-test-ssh/keys"), // subtree of ~/.suvadu-test-ssh
            Some(0),
            1_700_000_000_000,
            1_700_000_001_000,
        ))
        .unwrap();

        let mut mcp = default_mcp();
        mcp.exclude_dirs = vec!["~/.suvadu-test-ssh".to_string()];
        let text = call_tool(&repo, "search_commands", &json!({"query": ""}), &mcp).unwrap();
        assert!(
            !text.contains("ls -la"),
            "~-prefixed exclude_dirs should match its subtree: {text}"
        );
    }
}
