use crate::db;
use crate::repository;
use crate::stats_ui;
use crate::util::{dirs_home, format_count, format_duration_ms, shorten_path};

pub fn handle_stats_tui(days: Option<usize>, top: usize) -> Result<(), Box<dyn std::error::Error>> {
    let db_path = db::get_db_path()?;
    let conn = db::init_db(&db_path)?;
    let repo = repository::Repository::new(conn);

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
    let db_path = db::get_db_path()?;
    let conn = db::init_db(&db_path)?;
    let repo = repository::Repository::new(conn);
    let stats = repo.get_stats(days, top)?;

    let home = dirs_home();

    // Header
    let period = match stats.period_days {
        Some(d) => format!("last {d} days"),
        None => "all time".to_string(),
    };
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
    if !stats.top_commands.is_empty() {
        println!("\n\x1b[1m── Top Commands ──────────────────────────────────\x1b[0m");
        for (i, (cmd, count)) in stats.top_commands.iter().enumerate() {
            let pct = *count as f64 / stats.total_commands as f64 * 100.0;
            let truncated = if cmd.len() > 40 {
                format!("{}…", &cmd[..39])
            } else {
                cmd.clone()
            };
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
            let truncated = if short.len() > 45 {
                format!("…{}", &short[short.len() - 44..])
            } else {
                short
            };
            println!(
                "  {:>2}. {:<47} {:>6}",
                i + 1,
                truncated,
                format_count(*count)
            );
        }
    }

    // Executor breakdown
    if !stats.executor_breakdown.is_empty() {
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
