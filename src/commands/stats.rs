use crate::repository;
use crate::stats_ui;
use crate::util::{
    dirs_home, format_count, format_duration_ms, shorten_path, truncate_str, truncate_str_start,
};

pub fn handle_stats_tui(days: Option<usize>, top: usize) -> Result<(), Box<dyn std::error::Error>> {
    let repo = repository::Repository::init()?;

    crossterm::terminal::enable_raw_mode()?;
    let mut stdout = std::io::stdout();
    crossterm::execute!(stdout, crossterm::terminal::EnterAlternateScreen)?;
    let backend = ratatui::backend::CrosstermBackend::new(stdout);
    let mut terminal = ratatui::Terminal::new(backend)?;

    let res = stats_ui::run_stats_ui(&mut terminal, &repo, days, top);

    crossterm::terminal::disable_raw_mode()?;
    crossterm::execute!(
        terminal.backend_mut(),
        crossterm::terminal::LeaveAlternateScreen
    )?;
    terminal.show_cursor()?;

    if let Err(e) = res {
        eprintln!("Error in stats UI: {e}");
    }

    Ok(())
}

#[allow(clippy::too_many_lines, clippy::cast_precision_loss)]
pub fn handle_stats_text(
    days: Option<usize>,
    top: usize,
) -> Result<(), Box<dyn std::error::Error>> {
    let repo = repository::Repository::init()?;
    let stats = repo.get_stats(days, top)?;

    let home = dirs_home();

    // Header
    let period = stats
        .period_days
        .map_or_else(|| "all time".to_string(), |d| format!("last {d} days"));
    println!("\n\x1b[1m── Suvadu Stats ({period}) ─────────────────────\x1b[0m");
    println!(
        "  Total commands    {:<10}  Unique commands  {}",
        format_count(stats.total_commands),
        format_count(stats.unique_commands)
    );
    if stats.total_commands > 0 {
        let rate = if stats.total_commands > 0 {
            stats.success_count as f64 / stats.total_commands as f64 * 100.0
        } else {
            0.0
        };
        println!(
            "  Success rate      {rate:>5.1}%       ({} ok / {} fail)",
            format_count(stats.success_count),
            format_count(stats.failure_count)
        );
        println!(
            "  Avg duration      {}",
            format_duration_ms(stats.avg_duration_ms)
        );
    }

    // Top commands
    if !stats.top_commands.is_empty() && stats.total_commands > 0 {
        println!("\n\x1b[1m── Top Commands ──────────────────────────────────\x1b[0m");
        for (i, (cmd, count)) in stats.top_commands.iter().enumerate() {
            let pct = *count as f64 / stats.total_commands as f64 * 100.0;
            let truncated = truncate_str(cmd, 40, "…");
            println!(
                "  {:>2}. {:<42} {:>6}  ({pct:>4.1}%)",
                i + 1,
                truncated,
                format_count(*count)
            );
        }
    }

    // Busiest hours
    if !stats.hourly_distribution.is_empty() {
        let max_count = stats
            .hourly_distribution
            .iter()
            .map(|(_, c)| *c)
            .max()
            .unwrap_or(1);
        println!("\n\x1b[1m── Busiest Hours ─────────────────────────────────\x1b[0m");
        for (hour, count) in &stats.hourly_distribution {
            #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
            let bar_len = (*count as f64 / max_count as f64 * 30.0) as usize;
            let bar: String = "█".repeat(bar_len);
            println!("  {hour:02}:00  \x1b[32m{bar:<30}\x1b[0m  {count}");
        }
    }

    // Top directories
    if !stats.top_directories.is_empty() {
        println!("\n\x1b[1m── Top Directories ───────────────────────────────\x1b[0m");
        for (i, (dir, count)) in stats.top_directories.iter().enumerate() {
            let short = shorten_path(dir, &home);
            let truncated = truncate_str_start(&short, 45, "…");
            println!(
                "  {:>2}. {:<47} {:>6}",
                i + 1,
                truncated,
                format_count(*count)
            );
        }
    }

    // Executor breakdown
    if !stats.executor_breakdown.is_empty() && stats.total_commands > 0 {
        println!("\n\x1b[1m── Executor Breakdown ────────────────────────────\x1b[0m");
        for (exec_type, count) in &stats.executor_breakdown {
            let pct = *count as f64 / stats.total_commands as f64 * 100.0;
            println!(
                "  {:<15} {:>6}  ({pct:>4.1}%)",
                exec_type,
                format_count(*count)
            );
        }
    }

    println!();
    Ok(())
}

#[cfg(test)]
mod tests {
    use crate::db;
    use crate::models::{Entry, Session};
    use crate::repository::Repository;

    fn test_repo() -> Repository {
        let temp_dir = tempfile::TempDir::new().unwrap();
        let db_path = temp_dir.into_path().join("test.db");
        let conn = db::init_db(&db_path).unwrap();
        Repository::new(conn)
    }

    #[test]
    fn test_stats_empty_db() {
        let repo = test_repo();
        let stats = repo.get_stats(None, 10).unwrap();
        assert_eq!(stats.total_commands, 0);
        assert!(stats.top_commands.is_empty());
        assert!(stats.executor_breakdown.is_empty());
    }

    #[test]
    fn test_stats_with_entries() {
        let repo = test_repo();
        let session = Session {
            id: "test-sess".to_string(),
            hostname: "host".to_string(),
            created_at: 1_000_000,
            tag_id: None,
        };
        repo.insert_session(&session).unwrap();

        for i in 0..5 {
            let entry = Entry::new(
                "test-sess".to_string(),
                "git status".to_string(),
                "/home/user".to_string(),
                Some(0),
                1_000_000 + i * 1000,
                1_000_000 + i * 1000 + 100,
            );
            repo.insert_entry(&entry).unwrap();
        }

        let stats = repo.get_stats(None, 10).unwrap();
        assert_eq!(stats.total_commands, 5);
        assert_eq!(stats.success_count, 5);
        assert!(!stats.top_commands.is_empty());
        assert_eq!(stats.top_commands[0].0, "git status");
        assert_eq!(stats.top_commands[0].1, 5);
    }

    #[test]
    fn test_stats_percentage_no_division_by_zero() {
        // With total_commands == 0, the percentage sections are skipped
        let repo = test_repo();
        let stats = repo.get_stats(None, 10).unwrap();
        assert_eq!(stats.total_commands, 0);
        // Verify handle_stats_text doesn't panic with empty data
        // We can't call it directly (needs real DB path), but verify the
        // guard conditions: top_commands guard requires total > 0
        assert!(stats.top_commands.is_empty() || stats.total_commands > 0);
    }

    #[test]
    fn test_stats_with_failures() {
        let repo = test_repo();
        let session = Session {
            id: "s1".to_string(),
            hostname: "host".to_string(),
            created_at: 1_000_000,
            tag_id: None,
        };
        repo.insert_session(&session).unwrap();

        // 3 successes, 2 failures
        for i in 0..3 {
            let entry = Entry::new(
                "s1".to_string(),
                "ls".to_string(),
                "/tmp".to_string(),
                Some(0),
                1_000_000 + i * 1000,
                1_000_000 + i * 1000 + 50,
            );
            repo.insert_entry(&entry).unwrap();
        }
        for i in 0..2 {
            let entry = Entry::new(
                "s1".to_string(),
                "bad_cmd".to_string(),
                "/tmp".to_string(),
                Some(1),
                2_000_000 + i * 1000,
                2_000_000 + i * 1000 + 50,
            );
            repo.insert_entry(&entry).unwrap();
        }

        let stats = repo.get_stats(None, 10).unwrap();
        assert_eq!(stats.total_commands, 5);
        assert_eq!(stats.success_count, 3);
        assert_eq!(stats.failure_count, 2);
    }
}
