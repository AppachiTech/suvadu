use std::io;

use ratatui::backend::CrosstermBackend;
use ratatui::Terminal;

use crate::repository;
use crate::session_ui;
use crate::util;

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
    limit: usize,
) -> Result<(), Box<dyn std::error::Error>> {
    let repo = repository::Repository::init()?;

    match handle_session_with_repo(&repo, session_id, list, after, tag, limit)? {
        SessionResult::Empty | SessionResult::Listed => Ok(()),
        SessionResult::OpenSession(sid) => open_session_timeline(&repo, &sid),
        SessionResult::PickSession(sessions) => {
            // Interactive picker → timeline
            // RAII guard ensures terminal is restored even on panic
            let _guard = util::TerminalGuardMouse::new()?;
            let backend = CrosstermBackend::new(io::stdout());
            let mut terminal = Terminal::new(backend)?;

            let selected = session_ui::run_session_picker(&mut terminal, sessions);

            // If a session was selected, open its timeline
            let result = match selected {
                Ok(Some(sid)) => open_session_timeline_tui(&mut terminal, &repo, &sid),
                Ok(None) => Ok(()),
                Err(e) => Err(e.into()),
            };
            terminal.show_cursor()?;
            // _guard drops here, restoring terminal
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
    limit: usize,
) -> Result<SessionResult, Box<dyn std::error::Error>> {
    let tag_id = tag
        .map(|t| repo.get_tag_id_by_name(t))
        .transpose()?
        .flatten();

    let after_ms = after.and_then(|d| util::parse_date_input(d, false));

    // If a session ID was given directly, resolve by prefix
    if let Some(prefix) = session_id {
        let matches = repo.find_sessions_by_prefix(prefix)?;
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

    let sessions = repo.list_sessions(after_ms, tag_id, limit)?;

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
        "\n  {:<18} {:<10} {:<12} {:<10} {:>6} {:>6} {:>8}",
        "Date", "ID", "Host", "Tag", "Cmds", "Pass%", "Duration"
    );
    println!("  {}", "─".repeat(74));

    for s in sessions {
        let time = Local
            .timestamp_millis_opt(util::normalize_display_ms(s.created_at))
            .single()
            .map_or_else(
                || "????-??-?? ??:??".into(),
                |dt| dt.format("%Y-%m-%d %H:%M").to_string(),
            );
        let id_short: String = s.id.chars().take(8).collect();
        let tag_str = s.tag_name.as_deref().unwrap_or("—");

        #[allow(clippy::cast_precision_loss)]
        let rate = if s.cmd_count > 0 {
            s.success_count as f64 / s.cmd_count as f64 * 100.0
        } else {
            0.0
        };

        let duration = if s.last_cmd_at > s.first_cmd_at {
            format_duration_ms(s.last_cmd_at - s.first_cmd_at)
        } else {
            "—".into()
        };

        println!(
            "  {time:<18} {id_short:<10} {:<12} {tag_str:<10} {:>6} {:>5.0}% {:>8}",
            s.hostname, s.cmd_count, rate, duration
        );
    }
    println!();
}

fn open_session_timeline(
    repo: &repository::Repository,
    session_id: &str,
) -> Result<(), Box<dyn std::error::Error>> {
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
    let _guard = util::TerminalGuardMouse::new()?;
    let backend = CrosstermBackend::new(io::stdout());
    let mut terminal = Terminal::new(backend)?;

    let result =
        session_ui::run_session_timeline(&mut terminal, session, tag_name, entries, noted_ids);
    terminal.show_cursor()?;
    // _guard drops here, restoring terminal

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
                1_700_000_000_000 + (i as i64) * 1000,
                1_700_000_000_000 + (i as i64) * 1000 + 500,
            );
            repo.insert_entry(&entry).unwrap();
        }
    }

    #[test]
    fn test_session_list_empty() {
        let (_dir, repo) = test_repo();
        let result = handle_session_with_repo(&repo, None, true, None, None, 10);
        assert!(result.is_ok());
        assert!(matches!(result.unwrap(), SessionResult::Empty));
    }

    #[test]
    fn test_session_list_with_entries() {
        let (_dir, repo) = test_repo();
        seed_session_with_entries(&repo, "sess-1", 5);
        let result = handle_session_with_repo(&repo, None, true, None, None, 10).unwrap();
        assert!(matches!(result, SessionResult::Listed));
    }

    #[test]
    fn test_session_prefix_exact_match() {
        let (_dir, repo) = test_repo();
        seed_session_with_entries(&repo, "abc-unique-session", 3);
        let result =
            handle_session_with_repo(&repo, Some("abc-unique"), false, None, None, 10).unwrap();
        assert!(matches!(result, SessionResult::OpenSession(_)));
    }

    #[test]
    fn test_session_prefix_no_match() {
        let (_dir, repo) = test_repo();
        seed_session_with_entries(&repo, "abc-session", 3);
        let result = handle_session_with_repo(&repo, Some("xyz"), false, None, None, 10);
        assert!(result.is_err());
    }

    #[test]
    fn test_session_prefix_multiple_matches() {
        let (_dir, repo) = test_repo();
        seed_session_with_entries(&repo, "abc-session-1", 2);
        seed_session_with_entries(&repo, "abc-session-2", 2);
        let result = handle_session_with_repo(&repo, Some("abc"), false, None, None, 10);
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

        let result = handle_session_with_repo(&repo, None, true, None, Some("work"), 10).unwrap();
        assert!(matches!(result, SessionResult::Listed));
    }
}
