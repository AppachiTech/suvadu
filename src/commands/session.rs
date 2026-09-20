use std::io;

use ratatui::Terminal;

use crate::repository;
use crate::session_ui;
use crate::util;

const DEFAULT_LIST_LIMIT: usize = 50;

fn unified_summary(
    repo: &repository::Repository,
    session_id: &str,
) -> Result<crate::models::SessionSummary, Box<dyn std::error::Error>> {
    repo.list_unified_sessions(None, None, usize::MAX)?
        .into_iter()
        .find(|session| session.id == session_id)
        .ok_or_else(|| format!("Session {session_id} not found").into())
}

fn load_ai_session_data(
    repo: &repository::Repository,
    summary: crate::models::SessionSummary,
) -> Result<session_ui::AiSessionData, Box<dyn std::error::Error>> {
    let mut events = Vec::new();
    let mut offset = 0;
    loop {
        let page = repo.get_ai_session(&summary.id, 100, offset, &[])?;
        for event in page["events"].as_array().into_iter().flatten() {
            events.push(serde_json::from_value(event.clone())?);
        }
        let Some(next) = page["next_offset"].as_u64() else {
            break;
        };
        offset = usize::try_from(next)?;
    }
    let entries =
        repo.get_replay_entries(Some(&summary.id), &repository::ReplayFilter::default())?;
    let summaries = repo.ai_summaries_for_session(&summary.id)?;
    // The viewer explains an empty summary list differently depending on
    // whether an agent is even allowed to save one, so read the opt-in here
    // rather than guessing in the UI.
    let summary_writes_enabled =
        crate::config::load_config_cached().is_ok_and(|config| config.mcp.allow_session_summaries);
    Ok(session_ui::build_ai_session_data(
        summary,
        events,
        entries,
        summaries,
        summary_writes_enabled,
    ))
}

/// Result of the non-TUI session logic, used to decide what the caller should do.
#[derive(Debug)]
enum SessionResult {
    /// No sessions found at all.
    Empty,
    /// The `--list` path printed sessions to stdout.
    Listed,
    /// A single session matched; open it in the TUI timeline.
    OpenSession(String),
    /// Multiple sessions available; show the interactive picker.
    PickSession(Vec<crate::models::SessionSummary>),
}

pub fn handle_session(
    session_id: Option<&str>,
    list: bool,
    after: Option<&str>,
    tag: Option<&str>,
    limit: Option<u32>,
) -> Result<(), Box<dyn std::error::Error>> {
    let repo = repository::Repository::init()?;

    match handle_session_with_repo(&repo, session_id, list, after, tag, limit)? {
        SessionResult::Empty | SessionResult::Listed => Ok(()),
        SessionResult::OpenSession(sid) => open_session_timeline(&repo, &sid),
        SessionResult::PickSession(sessions) => {
            // Interactive picker → timeline, looping back to the picker after
            // closing a timeline instead of exiting suv sessions entirely —
            // Esc/q on the picker itself (no selection) is the only way out.
            // RAII guard ensures terminal is restored even on panic
            let mut guard = util::TerminalGuard::new()?;

            let result = loop {
                let selected = session_ui::run_session_picker(guard.terminal(), sessions.clone());
                match selected {
                    Ok(Some(sid)) => {
                        if let Err(e) = open_session_timeline_tui(guard.terminal(), &repo, &sid) {
                            break Err(e);
                        }
                    }
                    Ok(None) => break Ok(()),
                    Err(e) => break Err(e.into()),
                }
            };
            guard.terminal().show_cursor()?;
            result
        }
    }
}

fn handle_session_with_repo(
    repo: &repository::Repository,
    session_id: Option<&str>,
    list: bool,
    after: Option<&str>,
    tag: Option<&str>,
    limit: Option<u32>,
) -> Result<SessionResult, Box<dyn std::error::Error>> {
    let tag_id = tag
        .map(|t| repo.get_tag_id_by_name(t))
        .transpose()?
        .flatten();

    let after_ms = after.and_then(|d| util::parse_date_input(d, false));

    // If a session ID was given directly, resolve by prefix
    if let Some(prefix) = session_id {
        let matches = repo.find_unified_sessions_by_prefix(prefix)?;
        return match matches.len() {
            0 => Err(format!("No session found matching '{prefix}'").into()),
            1 => Ok(SessionResult::OpenSession(
                matches.into_iter().next().unwrap(),
            )),
            _ => {
                use std::fmt::Write;
                let mut msg = format!("Multiple sessions match '{prefix}':\n");
                for id in &matches {
                    let _ = writeln!(msg, "  {}", &id[..id.len().min(12)]);
                }
                msg.push_str("Provide a longer prefix to narrow it down.");
                Err(msg.into())
            }
        };
    }

    let effective_limit = limit.map_or_else(
        || {
            if list {
                DEFAULT_LIST_LIMIT
            } else {
                usize::MAX
            }
        },
        |value| usize::try_from(value).unwrap_or(usize::MAX),
    );
    let sessions = repo.list_unified_sessions(after_ms, tag_id, effective_limit)?;

    if sessions.is_empty() {
        println!("No sessions found.");
        return Ok(SessionResult::Empty);
    }

    // --list: print session list and exit
    if list {
        print_session_list(&sessions);
        return Ok(SessionResult::Listed);
    }

    Ok(SessionResult::PickSession(sessions))
}

fn print_session_list(sessions: &[crate::models::SessionSummary]) {
    use crate::util::format_duration_ms;
    use chrono::{Local, TimeZone};

    println!(
        "\n  {:<18} {:<10} {:<6} {:<12} {:<16} {:<18} {:>9} {:>6} {:>8}",
        "Last active", "ID", "Type", "Tag", "Agent / Host", "Model", "Tokens", "Cmds", "Duration"
    );
    println!("  {}", "─".repeat(116));

    for s in sessions {
        let time = Local
            .timestamp_millis_opt(util::normalize_display_ms(s.last_activity_at))
            .single()
            .map_or_else(
                || "????-??-?? ??:??".into(),
                |dt| dt.format("%Y-%m-%d %H:%M").to_string(),
            );
        let id_short: String = s.id.chars().take(8).collect();
        let duration = if s.last_activity_at > s.first_activity_at {
            format_duration_ms(s.last_activity_at - s.first_activity_at)
        } else {
            "—".into()
        };

        let actor = s.agent.as_deref().unwrap_or(&s.hostname);
        let tag = s.tag_name.as_deref().unwrap_or("—");
        let model = s.model.as_deref().unwrap_or("—");
        let tokens = s
            .total_tokens
            .map_or_else(|| "—".into(), |value| value.to_string());
        println!(
            "  {time:<18} {id_short:<10} {:<6} {tag:<12.12} {actor:<16.16} {model:<18.18} {tokens:>9} {:>6} {duration:>8}",
            s.kind, s.cmd_count
        );
    }
    println!();
}

fn open_session_timeline(
    repo: &repository::Repository,
    session_id: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    let summary = unified_summary(repo, session_id)?;
    if repo.has_native_ai_session(session_id)? {
        let data = load_ai_session_data(repo, summary)?;
        let mut guard = util::TerminalGuard::new()?;
        let result = session_ui::run_ai_session_timeline(guard.terminal(), data);
        guard.terminal().show_cursor()?;
        return result.map_err(Into::into);
    }
    let session = repo
        .get_session(session_id)?
        .ok_or_else(|| format!("Session {session_id} not found"))?;
    let tag_name = repo.get_tag_by_session(session_id)?;
    let entries =
        repo.get_replay_entries(Some(session_id), &repository::ReplayFilter::default())?;
    let noted_ids = repo.get_noted_entry_ids().unwrap_or_default();

    if entries.is_empty() {
        println!(
            "Session {} has no commands.",
            &session_id[..session_id.len().min(8)]
        );
        return Ok(());
    }

    // RAII guard ensures terminal is restored even on panic
    let mut guard = util::TerminalGuard::new()?;

    let result =
        session_ui::run_session_timeline(guard.terminal(), session, tag_name, entries, noted_ids);
    guard.terminal().show_cursor()?;

    result.map_err(Into::into)
}

fn open_session_timeline_tui<B: ratatui::backend::Backend>(
    terminal: &mut Terminal<B>,
    repo: &repository::Repository,
    session_id: &str,
) -> Result<(), Box<dyn std::error::Error>>
where
    io::Error: From<B::Error>,
{
    let summary = unified_summary(repo, session_id)?;
    if repo.has_native_ai_session(session_id)? {
        let data = load_ai_session_data(repo, summary)?;
        session_ui::run_ai_session_timeline(terminal, data)?;
        return Ok(());
    }
    let session = repo
        .get_session(session_id)?
        .ok_or_else(|| format!("Session {session_id} not found"))?;
    let tag_name = repo.get_tag_by_session(session_id)?;
    let entries =
        repo.get_replay_entries(Some(session_id), &repository::ReplayFilter::default())?;
    let noted_ids = repo.get_noted_entry_ids().unwrap_or_default();

    if entries.is_empty() {
        return Ok(());
    }

    session_ui::run_session_timeline(terminal, session, tag_name, entries, noted_ids)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::{Entry, Session};
    use crate::test_utils::test_repo;

    fn seed_session_with_entries(
        repo: &repository::Repository,
        session_id: &str,
        cmd_count: usize,
    ) {
        let session = Session {
            id: session_id.to_string(),
            hostname: "test-host".to_string(),
            created_at: 1_700_000_000_000,
            tag_id: None,
        };
        repo.insert_session(&session).unwrap();
        for i in 0..cmd_count {
            let entry = Entry::new(
                session_id.to_string(),
                format!("cmd-{i}"),
                "/tmp".to_string(),
                Some(0),
                1_700_000_000_000 + (i64::try_from(i).unwrap()) * 1000,
                1_700_000_000_000 + (i64::try_from(i).unwrap()) * 1000 + 500,
            );
            repo.insert_entry(&entry).unwrap();
        }
    }

    #[test]
    fn test_session_list_empty() {
        let (_dir, repo) = test_repo();
        let result = handle_session_with_repo(&repo, None, true, None, None, Some(10));
        assert!(result.is_ok());
        assert!(matches!(result.unwrap(), SessionResult::Empty));
    }

    #[test]
    fn test_session_list_with_entries() {
        let (_dir, repo) = test_repo();
        seed_session_with_entries(&repo, "sess-1", 5);
        let result = handle_session_with_repo(&repo, None, true, None, None, Some(10)).unwrap();
        assert!(matches!(result, SessionResult::Listed));
    }

    #[test]
    fn test_session_prefix_exact_match() {
        let (_dir, repo) = test_repo();
        seed_session_with_entries(&repo, "abc-unique-session", 3);
        let result =
            handle_session_with_repo(&repo, Some("abc-unique"), false, None, None, Some(10))
                .unwrap();
        assert!(matches!(result, SessionResult::OpenSession(_)));
    }

    #[test]
    fn test_session_prefix_no_match() {
        let (_dir, repo) = test_repo();
        seed_session_with_entries(&repo, "abc-session", 3);
        let result = handle_session_with_repo(&repo, Some("xyz"), false, None, None, Some(10));
        assert!(result.is_err());
    }

    #[test]
    fn test_session_prefix_multiple_matches() {
        let (_dir, repo) = test_repo();
        seed_session_with_entries(&repo, "abc-session-1", 2);
        seed_session_with_entries(&repo, "abc-session-2", 2);
        let result = handle_session_with_repo(&repo, Some("abc"), false, None, None, Some(10));
        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("Multiple sessions"));
    }

    #[test]
    fn test_session_list_with_tag_filter() {
        let (_dir, repo) = test_repo();
        repo.create_tag("work", None).unwrap();
        let tag_id = repo.get_tag_id_by_name("work").unwrap().unwrap();

        // Tagged session
        let session = Session {
            id: "tagged-sess".to_string(),
            hostname: "host".to_string(),
            created_at: 1_700_000_000_000,
            tag_id: Some(tag_id),
        };
        repo.insert_session(&session).unwrap();
        repo.tag_session("tagged-sess", Some(tag_id)).unwrap();
        let entry = Entry::new(
            "tagged-sess".to_string(),
            "cmd".to_string(),
            "/tmp".to_string(),
            Some(0),
            1_700_000_000_000,
            1_700_000_000_500,
        );
        repo.insert_entry(&entry).unwrap();

        // Untagged session
        seed_session_with_entries(&repo, "untagged-sess", 3);

        let result =
            handle_session_with_repo(&repo, None, true, None, Some("work"), Some(10)).unwrap();
        assert!(matches!(result, SessionResult::Listed));
    }

    #[test]
    fn interactive_picker_loads_more_than_the_list_default() {
        let (_dir, repo) = test_repo();
        for index in 0..51 {
            seed_session_with_entries(&repo, &format!("sess-{index:02}"), 1);
        }

        let result = handle_session_with_repo(&repo, None, false, None, None, None).unwrap();

        let SessionResult::PickSession(sessions) = result else {
            panic!("Expected interactive session picker");
        };
        assert_eq!(sessions.len(), 51);
    }

    #[test]
    fn interactive_picker_respects_an_explicit_limit() {
        let (_dir, repo) = test_repo();
        for index in 0..51 {
            seed_session_with_entries(&repo, &format!("sess-{index:02}"), 1);
        }

        let result = handle_session_with_repo(&repo, None, false, None, None, Some(20)).unwrap();

        let SessionResult::PickSession(sessions) = result else {
            panic!("Expected interactive session picker");
        };
        assert_eq!(sessions.len(), 20);
    }
}
