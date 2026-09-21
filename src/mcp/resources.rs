//! Read-only `suvadu://…` resources. These serve the same records the
//! tools do, so they answer in the same shape: see
//! [`crate::mcp::conventions`] for the rules and
//! [`crate::mcp::contract`] for the fixtures that enforce them on both
//! surfaces at once.

use serde_json::{json, Value};

use super::conventions as conv;
use crate::models::SearchField;
use crate::repository::{QueryFilter, Repository};

// ── Resource catalog ────────────────────────────────────────

/// Return the `resources/list` response.
pub fn list_resources(id: &Value, mcp: &crate::config::McpConfig) -> Value {
    let resources: Vec<Value> = super::catalog::RESOURCES
        .iter()
        .filter(|entry| super::catalog::resource_available(entry, mcp))
        .map(|entry| {
            json!({
                "uri": entry.uri(),
                "name": entry.title,
                "description": entry.description,
                "mimeType": "text/plain"
            })
        })
        .collect();
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "result": { "resources": resources }
    })
}

/// Return the `resources/templates/list` response.
pub fn list_resource_templates(id: &Value) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "result": {
            "resourceTemplates": [
                {
                    "uriTemplate": "suvadu://history/session/{session_id}",
                    "name": "Session History",
                    "description": "Full command history for a specific session",
                    "mimeType": "text/plain"
                }
            ]
        }
    })
}

// ── Resource reader ─────────────────────────────────────────

/// Read a resource by URI. Returns the text content or an error.
pub fn read_resource(
    repo: &Repository,
    uri: &str,
    mcp: &crate::config::McpConfig,
) -> Result<Value, String> {
    let suffix = uri.strip_prefix("suvadu://").unwrap_or(uri);
    // One gate, shared with `resources/list` and the settings UI, so a
    // resource can never be unadvertised yet still readable by URI.
    // Templated session URIs have no catalog entry; they mirror
    // `session_history` and follow that tool's state.
    let block = super::catalog::RESOURCES
        .iter()
        .find(|entry| entry.uri_suffix == suffix)
        .and_then(|entry| super::catalog::resource_block(entry, mcp))
        .or_else(|| {
            (suffix.starts_with("history/session/")
                && !super::catalog::tool_state_by_name("session_history", mcp).is_available())
            .then(|| {
                "it serves the same records as 'session_history', which is disabled".to_string()
            })
        });
    if let Some(reason) = block {
        return Err(format!("Resource '{uri}' is disabled: {reason}"));
    }
    let content = match uri {
        "suvadu://history/recent" => read_recent_history(repo, mcp)?,
        "suvadu://failures/recent" => read_recent_failures(repo, mcp)?,
        "suvadu://stats/today" => read_today_stats(repo, mcp)?,
        "suvadu://risk/summary" => read_risk_summary(repo, mcp)?,
        "suvadu://agents/activity" => read_agent_activity(repo, mcp)?,
        "suvadu://agents/sessions" => read_agent_sessions(repo, mcp)?,
        "suvadu://context/project" => read_project_context(repo, mcp)?,
        "suvadu://skills/index" => read_skills_index(repo, mcp)?,
        _ if uri.starts_with("suvadu://history/session/") => {
            let session_id = uri.strip_prefix("suvadu://history/session/").unwrap_or("");
            read_session_history(repo, session_id, mcp)?
        }
        _ => return Err(format!("Unknown resource: {uri}")),
    };

    Ok(json!({
        "contents": [{
            "uri": uri,
            "mimeType": "text/plain",
            "text": content
        }]
    }))
}

// ── Resource handlers ───────────────────────────────────────

/// Rows a fixed-shape resource shows before saying how many it elided.
/// Resources take no arguments, so these are the only page sizes there
/// are — the matching tool is the way to see more.
const MAX_FAILURES_SHOWN: usize = 20;
const MAX_SESSIONS_SHOWN: usize = 5;

/// One command row, matching the tool layer's exactly so an agent does
/// not have to learn two shapes for the same record.
fn command_row(e: &crate::models::Entry) -> String {
    format!(
        "  {} | {} | {} | {} | {} | {}",
        conv::command_id(e.id),
        conv::exit(e.exit_code),
        conv::timestamp(e.started_at),
        e.cwd,
        conv::or_unknown(e.executor.as_deref()),
        conv::clip(&e.command, conv::ROW_MAX_CHARS),
    )
}

fn read_recent_history(
    repo: &Repository,
    mcp: &crate::config::McpConfig,
) -> Result<String, String> {
    let limit = usize::try_from(mcp.default_limit).unwrap_or(20);
    let filter = QueryFilter {
        exclude_dirs: &mcp.exclude_dirs,
        ..QueryFilter::default()
    };
    let entries = repo
        .get_recent_entries(limit, 0, &filter, None)
        .map_err(|e| format!("query failed: {e}"))?;

    let mut response = conv::Response::new(
        format!("{} most recent commands", entries.len()),
        conv::Provenance::Observed,
    );
    for entry in &entries {
        response.line(command_row(entry));
    }
    Ok(response
        .shown(entries.len())
        .note("call recent_commands for paging, filters and detail")
        .render())
}

fn read_recent_failures(
    repo: &Repository,
    mcp: &crate::config::McpConfig,
) -> Result<String, String> {
    let now = chrono::Utc::now().timestamp_millis();
    let day_ago = now - 24 * 60 * 60 * 1000;

    let qf = QueryFilter {
        query_tokens: &[],
        after: Some(day_ago),
        before: None,
        tag_id: None,
        exit_code: None,
        query: None,
        prefix_match: false,
        executor: None,
        cwd: None,
        field: SearchField::Command,
        exclude_agents: false,
        cwd_prefix: false,
        failed_only: false,
        bookmarked_only: false,
        exclude_dirs: &mcp.exclude_dirs,
    };

    let entries = repo
        .get_entries_filtered(200, 0, &qf)
        .map_err(|e| format!("query failed: {e}"))?;

    let failures: Vec<_> = entries
        .iter()
        .filter(|e| e.exit_code.is_some_and(|c| c != 0))
        .collect();

    let mut response = conv::Response::new(
        format!(
            "{} commands failed in the {}",
            failures.len(),
            conv::window_hours(24)
        ),
        conv::Provenance::Observed,
    );
    for entry in failures.iter().take(MAX_FAILURES_SHOWN) {
        response.line(command_row(entry));
        if let Some(prompt) = entry
            .context
            .as_ref()
            .and_then(|ctx| ctx.get("agent_prompt"))
            .filter(|p| !p.is_empty())
        {
            response.line(format!(
                "    prompt \"{}\"",
                conv::clip(prompt, conv::PROMPT_MAX_CHARS)
            ));
        }
    }
    if failures.len() > MAX_FAILURES_SHOWN {
        response.line(format!(
            "  {}",
            conv::more_not_shown(failures.len() - MAX_FAILURES_SHOWN)
        ));
    }
    Ok(response
        .shown(failures.len().min(MAX_FAILURES_SHOWN))
        .matched(failures.len())
        .note(
            "the exit code is all suvadu recorded; the error text these commands printed was \
             never captured. Call what_failed for paging and grouping",
        )
        .render())
}

fn read_today_stats(repo: &Repository, mcp: &crate::config::McpConfig) -> Result<String, String> {
    let now = chrono::Utc::now().timestamp_millis();
    let today_start = now - (now % (24 * 60 * 60 * 1000));

    let qf = QueryFilter {
        query_tokens: &[],
        after: Some(today_start),
        before: None,
        tag_id: None,
        exit_code: None,
        query: None,
        prefix_match: false,
        executor: None,
        cwd: None,
        field: SearchField::Command,
        exclude_agents: false,
        cwd_prefix: false,
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

    let entries = repo
        .get_entries_filtered(100, 0, &qf)
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
    top_cmds.truncate(5);

    let mut top_dirs: Vec<_> = dir_counts.into_iter().collect();
    top_dirs.sort_by_key(|b| std::cmp::Reverse(b.1));
    top_dirs.truncate(3);

    let total_usize = usize::try_from(total).unwrap_or(usize::MAX);
    let mut response = conv::Response::new(
        format!(
            "Today: {total} commands, {} succeeded",
            conv::rate(usize::try_from(successes).unwrap_or(0), total_usize)
        ),
        conv::Provenance::ObservedAndInferred,
    );
    if !top_cmds.is_empty() {
        response.line("Top commands (ranked, inferred):");
        for (cmd, count) in &top_cmds {
            response.line(format!("    {count:>4}x  {cmd}"));
        }
    }
    if !top_dirs.is_empty() {
        response.blank();
        response.line("Top directories (ranked, inferred):");
        for (dir, count) in &top_dirs {
            response.line(format!("    {count:>4}x  {dir}"));
        }
    }
    Ok(response
        .shown(entries.len())
        .matched(total_usize)
        .note(format!(
            "rankings are computed from the {} most recent of {total} commands today, not from \
             all of them",
            entries.len()
        ))
        .render())
}

fn read_risk_summary(repo: &Repository, mcp: &crate::config::McpConfig) -> Result<String, String> {
    use crate::risk;

    let now = chrono::Utc::now().timestamp_millis();
    let day_ago = now - 24 * 60 * 60 * 1000;

    let qf = QueryFilter {
        query_tokens: &[],
        after: Some(day_ago),
        before: None,
        tag_id: None,
        exit_code: None,
        query: None,
        prefix_match: false,
        executor: None,
        cwd: None,
        field: SearchField::Command,
        exclude_agents: false,
        cwd_prefix: false,
        failed_only: false,
        bookmarked_only: false,
        exclude_dirs: &mcp.exclude_dirs,
    };

    let entries = repo
        .get_entries_filtered(500, 0, &qf)
        .map_err(|e| format!("query failed: {e}"))?;

    let risk_summary = risk::session_risk(&entries);

    let mut response = conv::Response::new(
        format!(
            "Risk ratings for {} commands recorded in the {}",
            entries.len(),
            conv::window_hours(24)
        ),
        conv::Provenance::Inferred,
    );
    for (label, count) in [
        ("critical", risk_summary.critical_count),
        ("high", risk_summary.high_count),
        ("medium", risk_summary.medium_count),
        ("low", risk_summary.low_count),
        ("safe", risk_summary.safe_count),
    ] {
        response.line(format!("  {label:<9} {count}"));
    }
    if !risk_summary.packages_installed.is_empty() {
        response.blank();
        response.line("Package installs (inferred from the command text):");
        for pkg in &risk_summary.packages_installed {
            response.line(format!("    {} ({})", pkg.packages.join(", "), pkg.manager));
        }
    }
    if !risk_summary.failed_commands.is_empty() {
        response.blank();
        response.line("Failed commands (observed exit codes):");
        for fail in risk_summary.failed_commands.iter().take(10) {
            response.line(format!(
                "    exit {} | {} | {}",
                fail.exit_code,
                conv::clip(&fail.command, conv::ROW_MAX_CHARS),
                conv::timestamp(fail.timestamp),
            ));
        }
        if risk_summary.failed_commands.len() > 10 {
            response.line(format!(
                "    {}",
                conv::more_not_shown(risk_summary.failed_commands.len() - 10)
            ));
        }
    }
    Ok(response
        .shown(entries.len())
        .matched(entries.len())
        .note(
            "a rating is a pattern match on the command text, not an execution or a sandbox; \
             suvadu did not observe what any of these commands touched",
        )
        .render())
}

fn read_agent_activity(
    repo: &Repository,
    mcp: &crate::config::McpConfig,
) -> Result<String, String> {
    let executors = repo
        .get_distinct_executors()
        .map_err(|e| format!("query failed: {e}"))?;

    let agents: Vec<&str> = executors
        .iter()
        .filter(|e| e.starts_with("agent:") && !e.ends_with("unknown"))
        .map(|e| e.strip_prefix("agent: ").unwrap_or(e.as_str()))
        .collect();

    let now = chrono::Utc::now().timestamp_millis();
    let week_ago = now - 7 * 24 * 60 * 60 * 1000;

    let mut response = conv::Response::new(
        format!(
            "{} agents recorded commands in the {}",
            agents.len(),
            conv::window_days(7)
        ),
        conv::Provenance::Observed,
    );

    for agent in &agents {
        let qf = QueryFilter {
            query_tokens: &[],
            after: Some(week_ago),
            before: None,
            tag_id: None,
            exit_code: None,
            query: None,
            prefix_match: false,
            executor: Some(agent),
            cwd: None,
            field: SearchField::Command,
            exclude_agents: false,
            cwd_prefix: false,
            failed_only: false,
            bookmarked_only: false,
            exclude_dirs: &mcp.exclude_dirs,
        };

        let total = repo.count_filtered(&qf).unwrap_or(0);
        let success_qf = QueryFilter {
            exit_code: Some(0),
            ..qf
        };
        let successes = repo.count_filtered(&success_qf).unwrap_or(0);
        response.line(format!(
            "  {agent} | {total} commands | {} ok",
            conv::rate(
                usize::try_from(successes).unwrap_or(0),
                usize::try_from(total).unwrap_or(0)
            )
        ));
    }

    Ok(response
        .shown(agents.len())
        .matched(agents.len())
        .note("an executor is recorded by the shell integration; commands run another way are not attributed to any agent")
        .render())
}

fn read_agent_sessions(
    repo: &Repository,
    mcp: &crate::config::McpConfig,
) -> Result<String, String> {
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
        cwd: None,
        field: SearchField::Command,
        exclude_agents: false,
        cwd_prefix: false,
        failed_only: false,
        bookmarked_only: false,
        exclude_dirs: &mcp.exclude_dirs,
    };

    let entries = repo
        .get_entries_filtered(5000, 0, &qf)
        .map_err(|e| format!("query failed: {e}"))?;

    let sessions = group_agent_sessions(&entries);
    let mut response = conv::Response::new(
        format!(
            "{} agent sessions in the {}",
            sessions.len(),
            conv::window_days(7)
        ),
        conv::Provenance::Observed,
    );
    for (sid, executor, count, success, failure, last_at, prompt) in
        sessions.iter().take(MAX_SESSIONS_SHOWN)
    {
        response.line(format!(
            "  {sid} | {executor} | {count} commands, {} ok, {failure} failed | last activity {}",
            conv::rate(*success, *count),
            conv::when(*last_at),
        ));
        if !prompt.is_empty() {
            response.line(format!(
                "    first prompt \"{}\"",
                conv::clip(prompt, conv::PROMPT_MAX_CHARS)
            ));
        }
    }
    if sessions.len() > MAX_SESSIONS_SHOWN {
        response.line(format!(
            "  {}",
            conv::more_not_shown(sessions.len() - MAX_SESSIONS_SHOWN)
        ));
    }
    Ok(response
        .shown(sessions.len().min(MAX_SESSIONS_SHOWN))
        .matched(sessions.len())
        .note("grouped from recorded shell commands; call find_agent_session for paging and filters, or list_agent_sessions for sessions captured from an agent's own transcript")
        .render())
}

/// Group agent entries by `session_id`, sorted by most recent.
fn group_agent_sessions(
    entries: &[crate::models::Entry],
) -> Vec<(&str, &str, usize, usize, usize, i64, String)> {
    let mut groups: std::collections::HashMap<&str, Vec<&crate::models::Entry>> =
        std::collections::HashMap::new();
    for entry in entries {
        if entry.executor_type.as_deref() != Some("agent") {
            continue;
        }
        groups
            .entry(entry.session_id.as_str())
            .or_default()
            .push(entry);
    }

    let mut sessions: Vec<_> = groups
        .into_iter()
        .map(|(session_id, cmds)| {
            let count = cmds.len();
            let success = cmds.iter().filter(|e| e.exit_code == Some(0)).count();
            let failure = cmds
                .iter()
                .filter(|e| e.exit_code.is_some_and(|c| c != 0))
                .count();
            let executor = cmds
                .first()
                .and_then(|e| e.executor.as_deref())
                .unwrap_or("unknown");
            let last_at = cmds.iter().map(|e| e.started_at).max().unwrap_or(0);
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
            (
                session_id,
                executor,
                count,
                success,
                failure,
                last_at,
                first_prompt,
            )
        })
        .collect();

    sessions.sort_by_key(|b| std::cmp::Reverse(b.5));
    sessions
}

fn high_fail_rate_rows(entries: &[crate::models::Entry], after: i64) -> Vec<String> {
    let day_entries: Vec<_> = entries.iter().filter(|e| e.started_at >= after).collect();
    if day_entries.is_empty() {
        return Vec::new();
    }
    let mut cmd_stats: std::collections::HashMap<&str, (usize, usize)> =
        std::collections::HashMap::new();
    for e in &day_entries {
        let entry = cmd_stats.entry(e.command.as_str()).or_insert((0, 0));
        entry.0 += 1;
        if e.exit_code.is_some_and(|c| c != 0) {
            entry.1 += 1;
        }
    }
    let mut high_fail: Vec<_> = cmd_stats
        .into_iter()
        .filter(|(_, (total, fails))| *total >= 3 && *fails * 100 / *total >= 50)
        .collect();
    high_fail.sort_by(|a, b| (b.1).1.cmp(&(a.1).1).then(a.0.cmp(b.0)));
    high_fail
        .into_iter()
        .map(|(cmd, (total, fails))| {
            format!(
                "    {} — failed {}",
                conv::clip(cmd, conv::ROW_MAX_CHARS),
                conv::rate(fails, total)
            )
        })
        .collect()
}

#[allow(clippy::too_many_lines)]
fn read_project_context(
    repo: &Repository,
    mcp: &crate::config::McpConfig,
) -> Result<String, String> {
    let now = chrono::Utc::now().timestamp_millis();
    let week_ago = now - 7 * 24 * 60 * 60 * 1000;
    let day_ago = now - 24 * 60 * 60 * 1000;

    // Get recent entries for this project (last 7 days)
    let qf = QueryFilter {
        after: Some(week_ago),
        exclude_dirs: &mcp.exclude_dirs,
        ..QueryFilter::default()
    };
    let entries = repo
        .get_entries_filtered(5000, 0, &qf)
        .map_err(|e| format!("query failed: {e}"))?;

    // Top commands by frequency
    let mut cmd_counts: std::collections::HashMap<&str, usize> = std::collections::HashMap::new();
    for e in &entries {
        let program = e.command.split_whitespace().next().unwrap_or(&e.command);
        *cmd_counts.entry(program).or_default() += 1;
    }
    let mut top_cmds: Vec<_> = cmd_counts.into_iter().collect();
    top_cmds.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(b.0)));
    top_cmds.truncate(8);

    let mut response = conv::Response::new(
        format!(
            "Project context — {} commands recorded in the {}",
            entries.len(),
            conv::window_days(7)
        ),
        conv::Provenance::ObservedAndInferred,
    );
    if !top_cmds.is_empty() {
        response.line("Common programs (inferred, ranked by frequency):");
        for (cmd, count) in &top_cmds {
            response.line(format!("    {count:>4}x  {cmd}"));
        }
    }

    // Recent failures (last 24h)
    let recent_failures: Vec<_> = entries
        .iter()
        .filter(|e| e.started_at >= day_ago && e.exit_code.is_some_and(|c| c != 0))
        .collect();

    if !recent_failures.is_empty() {
        // Group failures by command prefix
        let mut fail_counts: std::collections::HashMap<&str, (usize, i64)> =
            std::collections::HashMap::new();
        for e in &recent_failures {
            let entry = fail_counts.entry(e.command.as_str()).or_insert((0, 0));
            entry.0 += 1;
            if e.started_at > entry.1 {
                entry.1 = e.started_at;
            }
        }
        let mut sorted_fails: Vec<_> = fail_counts.into_iter().collect();
        sorted_fails.sort_by(|a, b| (b.1).0.cmp(&(a.1).0).then(a.0.cmp(b.0)));

        response.blank();
        response.line(format!(
            "Failures in the {} ({} recorded):",
            conv::window_hours(24),
            recent_failures.len()
        ));
        for (cmd, (count, last_at)) in sorted_fails.iter().take(5) {
            response.line(format!(
                "    {count}x  {} — last {}",
                conv::clip(cmd, conv::ROW_MAX_CHARS),
                conv::when(*last_at)
            ));
        }
        if sorted_fails.len() > 5 {
            response.line(format!(
                "    {}",
                conv::more_not_shown(sorted_fails.len() - 5)
            ));
        }
    }

    let high_fail = high_fail_rate_rows(&entries, day_ago);
    if !high_fail.is_empty() {
        response.blank();
        response.line(format!(
            "Commands failing half their runs or more in the {}:",
            conv::window_hours(24)
        ));
        for row in &high_fail {
            response.line(row);
        }
    }

    // Agent activity summary
    let agent_sessions = group_agent_sessions(&entries);
    if !agent_sessions.is_empty() {
        response.blank();
        response.line(format!(
            "Agent sessions in the {} ({}):",
            conv::window_days(7),
            agent_sessions.len()
        ));
        for (sid, executor, count, _success, failure, last_at, prompt) in
            agent_sessions.iter().take(3)
        {
            response.line(format!(
                "    {sid} | {executor} | {count} cmds, {failure} failed | last activity {}",
                conv::timestamp(*last_at)
            ));
            if !prompt.is_empty() {
                response.line(format!(
                    "      \"{}\"",
                    conv::clip(prompt, conv::PROMPT_MAX_CHARS)
                ));
            }
        }
        if agent_sessions.len() > 3 {
            response.line(format!(
                "    {}",
                conv::more_not_shown(agent_sessions.len() - 3)
            ));
        }
    }

    Ok(response
        .shown(entries.len())
        .note(format!(
            "computed over the {} most recent commands in this window; call project_context for \
             a directory-scoped briefing with detail",
            entries.len()
        ))
        .render())
}

fn read_skills_index(repo: &Repository, mcp: &crate::config::McpConfig) -> Result<String, String> {
    const MAX_SHOWN: usize = 30;
    // PROD-13: this resource mirrors `list_skills`, so it hides exactly
    // what that tool hides. A skill scoped to an excluded directory names
    // that directory in its scope — and often in its description too.
    let skills: Vec<_> = repo
        .list_skills(None, Some(crate::models::SKILL_STATUS_ACTIVE))
        .map_err(|e| format!("query failed: {e}"))?
        .into_iter()
        .filter(|skill| super::tools::skill_visible(skill, mcp))
        .collect();

    let mut response = conv::Response::new(
        format!("{} active shared skills", skills.len()),
        conv::Provenance::CallerReported,
    );
    if skills.is_empty() {
        response.line(
            "  (none — add one with `suv skills add <name>`, or via the propose_skill tool if \
             enabled)",
        );
    }
    for s in skills.iter().take(MAX_SHOWN) {
        response.line(format!(
            "  {} | scope {} | triggers {} | {}",
            s.name,
            s.scope,
            if s.triggers.is_empty() {
                conv::UNKNOWN.to_string()
            } else {
                s.triggers.join(", ")
            },
            conv::clip(&s.description, conv::PROMPT_MAX_CHARS)
        ));
    }
    if skills.len() > MAX_SHOWN {
        response.line(format!(
            "  {}",
            conv::more_not_shown(skills.len() - MAX_SHOWN)
        ));
    }
    Ok(response
        .shown(skills.len().min(MAX_SHOWN))
        .matched(skills.len())
        .note("skill text is written by people and agents, not observed by suvadu; call get_skill(name) for one skill's full body or search_skills to narrow")
        .render())
}

fn read_session_history(
    repo: &Repository,
    session_id: &str,
    mcp: &crate::config::McpConfig,
) -> Result<String, String> {
    if session_id.is_empty() {
        return Err("session_id is required".to_string());
    }

    let entries = repo
        .get_replay_entries(
            Some(session_id),
            &crate::repository::ReplayFilter {
                limit: Some(100),
                exclude_dirs: &mcp.exclude_dirs,
                ..Default::default()
            },
        )
        .map_err(|e| format!("query failed: {e}"))?;

    let mut response = conv::Response::new(
        format!("Session {session_id} — {} commands", entries.len()),
        conv::Provenance::Observed,
    );
    for entry in &entries {
        response.line(command_row(entry));
    }
    Ok(response
        .shown(entries.len())
        .next_offset((entries.len() == 100).then_some(100))
        .note("at most 100 commands; call session_history for paging and detail")
        .render())
}

// ── Tests ───────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_list_resources_advertises_the_whole_catalog() {
        let mcp = crate::config::McpConfig::default();
        let resp = list_resources(&json!(1), &mcp);
        let resources = resp["result"]["resources"].as_array().unwrap();
        for r in resources {
            assert!(r["uri"].is_string());
            assert!(r["name"].is_string());
            assert!(r["mimeType"].is_string());
        }
        let advertised: std::collections::BTreeSet<String> = resources
            .iter()
            .map(|r| r["uri"].as_str().unwrap().to_string())
            .collect();
        let cataloged: std::collections::BTreeSet<String> = super::super::catalog::RESOURCES
            .iter()
            .map(super::super::catalog::ResourceEntry::uri)
            .collect();
        assert_eq!(advertised, cataloged);
    }

    #[test]
    fn every_catalog_resource_has_a_reader() {
        let (_dir, repo) = crate::test_utils::test_repo();
        let mcp = crate::config::McpConfig::default();
        for entry in super::super::catalog::RESOURCES {
            let uri = entry.uri();
            let result = read_resource(&repo, &uri, &mcp);
            assert!(
                result.is_ok(),
                "catalog resource {uri} has no reader: {result:?}"
            );
        }
    }

    #[test]
    fn every_catalog_resource_can_be_disabled() {
        let (_dir, repo) = crate::test_utils::test_repo();
        for entry in super::super::catalog::RESOURCES {
            let mcp = crate::config::McpConfig {
                disabled_resources: vec![entry.uri_suffix.to_string()],
                ..Default::default()
            };
            let resp = list_resources(&json!(1), &mcp);
            let still_listed = resp["result"]["resources"]
                .as_array()
                .unwrap()
                .iter()
                .any(|r| r["uri"].as_str() == Some(entry.uri().as_str()));
            assert!(
                !still_listed,
                "{} is still advertised while disabled",
                entry.uri_suffix
            );
            let err = read_resource(&repo, &entry.uri(), &mcp)
                .expect_err("a disabled resource must not be readable");
            assert!(err.contains("disabled"), "{err}");
        }
    }

    #[test]
    fn test_list_resources_with_disabled() {
        let mcp = crate::config::McpConfig {
            disabled_resources: vec!["context/project".to_string(), "risk/summary".to_string()],
            ..Default::default()
        };
        let resp = list_resources(&json!(1), &mcp);
        let resources = resp["result"]["resources"].as_array().unwrap();
        assert_eq!(resources.len(), 6);
        let uris: Vec<&str> = resources
            .iter()
            .map(|r| r["uri"].as_str().unwrap())
            .collect();
        assert!(!uris.contains(&"suvadu://context/project"));
        assert!(!uris.contains(&"suvadu://risk/summary"));
        assert!(uris.contains(&"suvadu://history/recent"));
    }

    #[test]
    fn test_read_disabled_resource_returns_error() {
        let (_dir, repo) = crate::test_utils::test_repo();
        let mcp = crate::config::McpConfig {
            disabled_resources: vec!["history/recent".to_string()],
            ..Default::default()
        };
        let result = read_resource(&repo, "suvadu://history/recent", &mcp);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("disabled"));
    }

    #[test]
    fn test_list_templates_has_session() {
        let resp = list_resource_templates(&json!(1));
        let templates = resp["result"]["resourceTemplates"].as_array().unwrap();
        assert_eq!(templates.len(), 1);
        assert!(templates[0]["uriTemplate"]
            .as_str()
            .unwrap()
            .contains("{session_id}"));
    }

    #[test]
    fn test_read_recent_history_empty() {
        let (_dir, repo) = crate::test_utils::test_repo();
        let result = read_resource(
            &repo,
            "suvadu://history/recent",
            &crate::config::McpConfig::default(),
        );
        assert!(result.is_ok());
        let val = result.unwrap();
        assert!(val["contents"][0]["text"]
            .as_str()
            .unwrap()
            .contains("shown: 0"));
    }

    #[test]
    fn test_read_failures_empty() {
        let (_dir, repo) = crate::test_utils::test_repo();
        let result = read_resource(
            &repo,
            "suvadu://failures/recent",
            &crate::config::McpConfig::default(),
        );
        assert!(result.is_ok());
        assert!(result.unwrap()["contents"][0]["text"]
            .as_str()
            .unwrap()
            .contains("shown: 0"));
    }

    #[test]
    fn test_read_stats_empty() {
        let (_dir, repo) = crate::test_utils::test_repo();
        let result = read_resource(
            &repo,
            "suvadu://stats/today",
            &crate::config::McpConfig::default(),
        );
        assert!(result.is_ok());
        assert!(result.unwrap()["contents"][0]["text"]
            .as_str()
            .unwrap()
            .contains("Today: 0 commands"));
    }

    #[test]
    fn test_read_risk_empty() {
        let (_dir, repo) = crate::test_utils::test_repo();
        let result = read_resource(
            &repo,
            "suvadu://risk/summary",
            &crate::config::McpConfig::default(),
        );
        assert!(result.is_ok());
    }

    #[test]
    fn test_read_agents_empty() {
        let (_dir, repo) = crate::test_utils::test_repo();
        let result = read_resource(
            &repo,
            "suvadu://agents/activity",
            &crate::config::McpConfig::default(),
        );
        assert!(result.is_ok());
        assert!(result.unwrap()["contents"][0]["text"]
            .as_str()
            .unwrap()
            .contains("shown: 0"));
    }

    #[test]
    fn test_read_agent_sessions_empty() {
        let (_dir, repo) = crate::test_utils::test_repo();
        let result = read_resource(
            &repo,
            "suvadu://agents/sessions",
            &crate::config::McpConfig::default(),
        );
        assert!(result.is_ok());
        assert!(result.unwrap()["contents"][0]["text"]
            .as_str()
            .unwrap()
            .contains("shown: 0"));
    }

    #[test]
    fn test_read_agent_sessions_with_data() {
        let (_dir, repo) = crate::test_utils::test_repo();

        let session = crate::models::Session {
            id: "claude-test1".into(),
            hostname: "test".into(),
            created_at: chrono::Utc::now().timestamp_millis() - 3_600_000,
            tag_id: None,
        };
        repo.insert_session(&session).unwrap();

        let mut entry = crate::models::Entry::new(
            "claude-test1".into(),
            "cargo test".into(),
            "/project".into(),
            Some(0),
            chrono::Utc::now().timestamp_millis() - 3_600_000,
            chrono::Utc::now().timestamp_millis() - 3_599_000,
        );
        let mut ctx = std::collections::HashMap::new();
        ctx.insert("agent_prompt".into(), "run tests".into());
        entry.context = Some(ctx);
        entry.executor_type = Some("agent".into());
        entry.executor = Some("claude-code".into());
        repo.insert_entry(&entry).unwrap();

        let result = read_resource(
            &repo,
            "suvadu://agents/sessions",
            &crate::config::McpConfig::default(),
        );
        assert!(result.is_ok());
        let text = result.unwrap()["contents"][0]["text"]
            .as_str()
            .unwrap()
            .to_string();
        assert!(
            text.contains("claude-test1"),
            "should contain session id: {text}"
        );
        assert!(
            text.contains("claude-code"),
            "should contain executor: {text}"
        );
        assert!(text.contains("run tests"), "should contain prompt: {text}");
    }

    #[test]
    fn test_read_project_context_empty() {
        let (_dir, repo) = crate::test_utils::test_repo();
        let result = read_resource(
            &repo,
            "suvadu://context/project",
            &crate::config::McpConfig::default(),
        );
        assert!(result.is_ok());
        assert!(result.unwrap()["contents"][0]["text"]
            .as_str()
            .unwrap()
            .contains("0 commands recorded"));
    }

    #[test]
    fn test_read_project_context_with_data() {
        let (_dir, repo) = crate::test_utils::test_repo();

        let session = crate::models::Session {
            id: "claude-ctx1".into(),
            hostname: "test".into(),
            created_at: chrono::Utc::now().timestamp_millis() - 3_600_000,
            tag_id: None,
        };
        repo.insert_session(&session).unwrap();

        let now = chrono::Utc::now().timestamp_millis();
        for i in 0..5 {
            let mut entry = crate::models::Entry::new(
                "claude-ctx1".into(),
                "cargo test".into(),
                "/project".into(),
                Some(i32::from(i >= 3)),
                now - (i * 60_000) - 3_600_000,
                now - (i * 60_000) - 3_599_000,
            );
            entry.executor_type = Some("agent".into());
            entry.executor = Some("claude-code".into());
            repo.insert_entry(&entry).unwrap();
        }

        let result = read_resource(
            &repo,
            "suvadu://context/project",
            &crate::config::McpConfig::default(),
        );
        assert!(result.is_ok());
        let text = result.unwrap()["contents"][0]["text"]
            .as_str()
            .unwrap()
            .to_string();
        assert!(
            text.contains("Common programs"),
            "should show common commands: {text}"
        );
        assert!(text.contains("cargo"), "should contain cargo: {text}");
    }

    #[test]
    fn test_read_skills_index_empty() {
        let (_dir, repo) = crate::test_utils::test_repo();
        let result = read_resource(
            &repo,
            "suvadu://skills/index",
            &crate::config::McpConfig::default(),
        );
        assert!(result.is_ok());
        assert!(result.unwrap()["contents"][0]["text"]
            .as_str()
            .unwrap()
            .contains("shown: 0"));
    }

    #[test]
    fn test_read_skills_index_with_data() {
        let (_dir, repo) = crate::test_utils::test_repo();
        repo.create_skill(&crate::models::NewSkill {
            name: "deploy-checklist".into(),
            description: "Steps before a deploy".into(),
            body: "1. run tests\n2. tag release".into(),
            triggers: vec!["deploy".into()],
            scope: crate::models::SKILL_SCOPE_GLOBAL.into(),
            source: crate::models::SKILL_SOURCE_HUMAN.into(),
            status: crate::models::SKILL_STATUS_ACTIVE.into(),
        })
        .unwrap();
        repo.create_skill(&crate::models::NewSkill {
            name: "hidden-draft".into(),
            description: "not ready".into(),
            body: "wip".into(),
            triggers: vec![],
            scope: crate::models::SKILL_SCOPE_GLOBAL.into(),
            source: "agent:claude-code".into(),
            status: crate::models::SKILL_STATUS_PENDING.into(),
        })
        .unwrap();

        let result = read_resource(
            &repo,
            "suvadu://skills/index",
            &crate::config::McpConfig::default(),
        );
        let text = result.unwrap()["contents"][0]["text"]
            .as_str()
            .unwrap()
            .to_string();
        assert!(text.contains("deploy-checklist"));
        assert!(text.contains("Steps before a deploy"));
        assert!(
            !text.contains("hidden-draft"),
            "pending skills must not appear: {text}"
        );
    }

    #[test]
    fn test_read_unknown_resource() {
        let (_dir, repo) = crate::test_utils::test_repo();
        let result = read_resource(
            &repo,
            "suvadu://nonexistent",
            &crate::config::McpConfig::default(),
        );
        assert!(result.is_err());
    }

    #[test]
    fn test_read_session_empty_id() {
        let (_dir, repo) = crate::test_utils::test_repo();
        let result = read_resource(
            &repo,
            "suvadu://history/session/",
            &crate::config::McpConfig::default(),
        );
        assert!(result.is_err());
    }

    #[test]
    fn test_read_recent_with_data() {
        let (_dir, repo) = crate::test_utils::test_repo();
        let session = crate::models::Session {
            id: "s1".to_string(),
            hostname: "test".to_string(),
            created_at: chrono::Utc::now().timestamp_millis(),
            tag_id: None,
        };
        repo.insert_session(&session).unwrap();

        let entry = crate::models::Entry::new(
            "s1".to_string(),
            "cargo test".to_string(),
            "/project".to_string(),
            Some(0),
            chrono::Utc::now().timestamp_millis() - 1000,
            chrono::Utc::now().timestamp_millis(),
        );
        repo.insert_entry(&entry).unwrap();

        let result = read_resource(
            &repo,
            "suvadu://history/recent",
            &crate::config::McpConfig::default(),
        );
        assert!(result.is_ok());
        let text = result.unwrap()["contents"][0]["text"]
            .as_str()
            .unwrap()
            .to_string();
        assert!(
            text.contains("cargo test"),
            "should contain command: {text}"
        );
        assert!(text.contains("/project"), "should contain dir: {text}");
    }

    #[test]
    fn test_read_resource_respects_exclude_dirs() {
        // mcp.exclude_dirs was documented and configurable but never enforced
        // by any resource handler before this test existed.
        let (_dir, repo) = crate::test_utils::test_repo();
        let session = crate::models::Session {
            id: "s-excl".to_string(),
            hostname: "test".to_string(),
            created_at: chrono::Utc::now().timestamp_millis(),
            tag_id: None,
        };
        repo.insert_session(&session).unwrap();
        repo.insert_entry(&crate::models::Entry::new(
            "s-excl".to_string(),
            "cat id_rsa".to_string(),
            "/Users/test/.ssh".to_string(),
            Some(0),
            chrono::Utc::now().timestamp_millis() - 1000,
            chrono::Utc::now().timestamp_millis(),
        ))
        .unwrap();
        repo.insert_entry(&crate::models::Entry::new(
            "s-excl".to_string(),
            "cargo build".to_string(),
            "/Users/test/project".to_string(),
            Some(0),
            chrono::Utc::now().timestamp_millis() - 500,
            chrono::Utc::now().timestamp_millis(),
        ))
        .unwrap();

        let mcp = crate::config::McpConfig {
            exclude_dirs: vec!["/Users/test/.ssh".to_string()],
            ..Default::default()
        };

        let result = read_resource(&repo, "suvadu://history/recent", &mcp);
        let text = result.unwrap()["contents"][0]["text"]
            .as_str()
            .unwrap()
            .to_string();
        assert!(text.contains("cargo build"));
        assert!(!text.contains("id_rsa"), "excluded dir leaked: {text}");
    }
}
