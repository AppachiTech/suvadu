use std::io;

use crossterm::event::{DisableMouseCapture, EnableMouseCapture};
use crossterm::execute;
use crossterm::terminal::{EnterAlternateScreen, LeaveAlternateScreen};
use ratatui::backend::CrosstermBackend;
use ratatui::Terminal;

use crate::repository;
use crate::session_ui;
use crate::util;

pub fn handle_session(
    session_id: Option<&str>,
    list: bool,
    after: Option<&str>,
    tag: Option<&str>,
    limit: usize,
) -> Result<(), Box<dyn std::error::Error>> {
    let repo = repository::Repository::init()?;

    let tag_id = tag
        .map(|t| repo.get_tag_id_by_name(t))
        .transpose()?
        .flatten();

    let after_ms = after.and_then(|d| util::parse_date_input(d, false));

    // If a session ID was given directly, open that session
    if let Some(prefix) = session_id {
        let matches = repo.find_sessions_by_prefix(prefix)?;
        match matches.len() {
            0 => {
                eprintln!("No session found matching '{prefix}'");
                std::process::exit(1);
            }
            1 => return open_session_timeline(&repo, &matches[0]),
            _ => {
                eprintln!("Multiple sessions match '{prefix}':");
                for id in &matches {
                    eprintln!("  {}", &id[..id.len().min(12)]);
                }
                eprintln!("Provide a longer prefix to narrow it down.");
                std::process::exit(1);
            }
        }
    }

    let sessions = repo.list_sessions(after_ms, tag_id, limit)?;

    if sessions.is_empty() {
        println!("No sessions found.");
        return Ok(());
    }

    // --list: print session list and exit
    if list {
        print_session_list(&sessions);
        return Ok(());
    }

    // Interactive picker → timeline
    crossterm::terminal::enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen, EnableMouseCapture)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let selected = session_ui::run_session_picker(&mut terminal, sessions);

    // If a session was selected, open its timeline
    match selected {
        Ok(Some(sid)) => {
            // Stay in alternate screen for the timeline
            let result = open_session_timeline_tui(&mut terminal, &repo, &sid);
            crossterm::terminal::disable_raw_mode()?;
            execute!(
                terminal.backend_mut(),
                LeaveAlternateScreen,
                DisableMouseCapture
            )?;
            terminal.show_cursor()?;
            result
        }
        Ok(None) => {
            // User quit the picker
            crossterm::terminal::disable_raw_mode()?;
            execute!(
                terminal.backend_mut(),
                LeaveAlternateScreen,
                DisableMouseCapture
            )?;
            terminal.show_cursor()?;
            Ok(())
        }
        Err(e) => {
            crossterm::terminal::disable_raw_mode()?;
            execute!(
                terminal.backend_mut(),
                LeaveAlternateScreen,
                DisableMouseCapture
            )?;
            terminal.show_cursor()?;
            Err(e.into())
        }
    }
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
            .timestamp_millis_opt(s.created_at)
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
    let entries = repo.get_replay_entries(Some(session_id), None, None, None, None, None, None)?;
    let noted_ids = repo.get_noted_entry_ids().unwrap_or_default();

    if entries.is_empty() {
        println!(
            "Session {} has no commands.",
            &session_id[..session_id.len().min(8)]
        );
        return Ok(());
    }

    crossterm::terminal::enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen, EnableMouseCapture)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let result =
        session_ui::run_session_timeline(&mut terminal, session, tag_name, entries, noted_ids);

    crossterm::terminal::disable_raw_mode()?;
    execute!(
        terminal.backend_mut(),
        LeaveAlternateScreen,
        DisableMouseCapture
    )?;
    terminal.show_cursor()?;

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
    let entries = repo.get_replay_entries(Some(session_id), None, None, None, None, None, None)?;
    let noted_ids = repo.get_noted_entry_ids().unwrap_or_default();

    if entries.is_empty() {
        return Ok(());
    }

    session_ui::run_session_timeline(terminal, session, tag_name, entries, noted_ids)?;
    Ok(())
}
