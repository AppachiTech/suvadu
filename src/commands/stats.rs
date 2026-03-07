use crate::models::Stats;
use crate::repository;
use crate::stats_ui;
use crate::util::{
    self, dirs_home, format_count, format_duration_ms, shorten_path, truncate_str,
    truncate_str_start,
};

pub fn handle_stats_tui(
    days: Option<usize>,
    top: usize,
    tag: Option<&str>,
) -> Result<(), Box<dyn std::error::Error>> {
    let repo = repository::Repository::init()?;

    let tag_id = resolve_tag(&repo, tag)?;

    let mut guard = crate::util::TerminalGuard::new()?;
    let res = stats_ui::run_stats_ui(guard.terminal(), &repo, days, top, tag_id, tag);
    drop(guard);

    res?;
    Ok(())
}

/// Resolve a tag name to its ID, returning an error message if not found.
fn resolve_tag(
    repo: &repository::Repository,
    tag: Option<&str>,
) -> Result<Option<i64>, Box<dyn std::error::Error>> {
    let Some(name) = tag else {
        return Ok(None);
    };
    let tag_id = repo.get_tag_id_by_name(name)?;
    if tag_id.is_none() {
        return Err(
            format!("Tag '{name}' not found. Use `suv tag list` to see available tags.").into(),
        );
    }
    Ok(tag_id)
}

#[allow(clippy::cast_precision_loss)]
pub fn handle_stats_text(
    days: Option<usize>,
    top: usize,
    tag: Option<&str>,
) -> Result<(), Box<dyn std::error::Error>> {
    let repo = repository::Repository::init()?;

    let tag_id = resolve_tag(&repo, tag)?;
    let stats = repo.get_stats(days, top, tag_id)?;

    // Header
    let period = stats
        .period_days
        .map_or_else(|| "all time".to_string(), |d| format!("last {d} days"));
    let tag_label = tag.map_or_else(String::new, |t| format!(", tag: {t}"));
    let c = util::color_enabled();
    let (b, r) = if c { ("\x1b[1m", "\x1b[0m") } else { ("", "") };
    println!("\n{b}── Suvadu Stats ({period}{tag_label}) ─────────────────────{r}");
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

    print_top_commands(&stats);
    print_hourly_distribution(&stats);
    print_top_directories(&stats);
    print_executor_breakdown(&stats);

    println!();
    Ok(())
}

#[allow(clippy::cast_precision_loss)]
fn print_top_commands(stats: &Stats) {
    if stats.top_commands.is_empty() || stats.total_commands == 0 {
        return;
    }
    let (b, r) = if util::color_enabled() {
        ("\x1b[1m", "\x1b[0m")
    } else {
        ("", "")
    };
    println!("\n{b}── Top Commands ──────────────────────────────────{r}");
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

#[allow(clippy::cast_precision_loss)]
fn print_hourly_distribution(stats: &Stats) {
    if stats.hourly_distribution.is_empty() {
        return;
    }
    let max_count = stats
        .hourly_distribution
        .iter()
        .map(|(_, c)| *c)
        .max()
        .unwrap_or(1);
    let c = util::color_enabled();
    let (b, r) = if c { ("\x1b[1m", "\x1b[0m") } else { ("", "") };
    println!("\n{b}── Busiest Hours ─────────────────────────────────{r}");
    for (hour, count) in &stats.hourly_distribution {
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let bar_len = (*count as f64 / max_count as f64 * 30.0) as usize;
        let bar: String = "█".repeat(bar_len);
        if c {
            println!("  {hour:02}:00  \x1b[32m{bar:<30}\x1b[0m  {count}");
        } else {
            println!("  {hour:02}:00  {bar:<30}  {count}");
        }
    }
}

fn print_top_directories(stats: &Stats) {
    if stats.top_directories.is_empty() {
        return;
    }
    let home = dirs_home();
    let (b, r) = if util::color_enabled() {
        ("\x1b[1m", "\x1b[0m")
    } else {
        ("", "")
    };
    println!("\n{b}── Top Directories ───────────────────────────────{r}");
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

#[allow(clippy::cast_precision_loss)]
fn print_executor_breakdown(stats: &Stats) {
    if stats.executor_breakdown.is_empty() || stats.total_commands == 0 {
        return;
    }
    let (b, r) = if util::color_enabled() {
        ("\x1b[1m", "\x1b[0m")
    } else {
        ("", "")
    };
    println!("\n{b}── Executor Breakdown ────────────────────────────{r}");
    for (exec_type, count) in &stats.executor_breakdown {
        let pct = *count as f64 / stats.total_commands as f64 * 100.0;
        println!(
            "  {:<15} {:>6}  ({pct:>4.1}%)",
            exec_type,
            format_count(*count)
        );
    }
}

#[cfg(test)]
mod tests {
    use crate::db;
    use crate::models::{Entry, Session};
    use crate::repository::Repository;

    fn test_repo() -> (tempfile::TempDir, Repository) {
        let temp_dir = tempfile::TempDir::new().unwrap();
        let db_path = temp_dir.path().join("test.db");
        let conn = db::init_db(&db_path).unwrap();
        (temp_dir, Repository::new(conn))
    }

    #[test]
    fn test_stats_empty_db() {
        let (_dir, repo) = test_repo();
        let stats = repo.get_stats(None, 10, None).unwrap();
        assert_eq!(stats.total_commands, 0);
        assert!(stats.top_commands.is_empty());
        assert!(stats.executor_breakdown.is_empty());
    }

    #[test]
    fn test_stats_with_entries() {
        let (_dir, repo) = test_repo();
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

        let stats = repo.get_stats(None, 10, None).unwrap();
        assert_eq!(stats.total_commands, 5);
        assert_eq!(stats.success_count, 5);
        assert!(!stats.top_commands.is_empty());
        assert_eq!(stats.top_commands[0].0, "git status");
        assert_eq!(stats.top_commands[0].1, 5);
    }

    #[test]
    fn test_stats_percentage_no_division_by_zero() {
        let (_dir, repo) = test_repo();
        let stats = repo.get_stats(None, 10, None).unwrap();
        assert_eq!(stats.total_commands, 0);
        assert!(stats.top_commands.is_empty() || stats.total_commands > 0);
    }

    #[test]
    fn test_stats_with_failures() {
        let (_dir, repo) = test_repo();
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

        let stats = repo.get_stats(None, 10, None).unwrap();
        assert_eq!(stats.total_commands, 5);
        assert_eq!(stats.success_count, 3);
        assert_eq!(stats.failure_count, 2);
    }

    #[test]
    fn test_stats_filtered_by_tag() {
        let (_dir, repo) = test_repo();

        // Create a tag
        repo.create_tag("work", None).unwrap();
        let tag_id = repo.get_tag_id_by_name("work").unwrap().unwrap();

        // Session with the tag
        let tagged_session = Session {
            id: "tagged-sess".to_string(),
            hostname: "host".to_string(),
            created_at: 1_000_000,
            tag_id: Some(tag_id),
        };
        repo.insert_session(&tagged_session).unwrap();
        repo.tag_session("tagged-sess", Some(tag_id)).unwrap();

        // Session without the tag
        let untagged_session = Session {
            id: "untagged-sess".to_string(),
            hostname: "host".to_string(),
            created_at: 1_000_000,
            tag_id: None,
        };
        repo.insert_session(&untagged_session).unwrap();

        // 3 entries in tagged session
        for i in 0..3 {
            let entry = Entry::new(
                "tagged-sess".to_string(),
                "cargo build".to_string(),
                "/project".to_string(),
                Some(0),
                1_000_000 + i * 1000,
                1_000_000 + i * 1000 + 100,
            );
            repo.insert_entry(&entry).unwrap();
        }

        // 2 entries in untagged session
        for i in 0..2 {
            let entry = Entry::new(
                "untagged-sess".to_string(),
                "ls".to_string(),
                "/tmp".to_string(),
                Some(0),
                2_000_000 + i * 1000,
                2_000_000 + i * 1000 + 50,
            );
            repo.insert_entry(&entry).unwrap();
        }

        // Unfiltered: all 5 entries
        let all_stats = repo.get_stats(None, 10, None).unwrap();
        assert_eq!(all_stats.total_commands, 5);

        // Filtered by tag: only 3 entries from tagged session
        let tag_stats = repo.get_stats(None, 10, Some(tag_id)).unwrap();
        assert_eq!(tag_stats.total_commands, 3);
        assert_eq!(tag_stats.success_count, 3);
        assert_eq!(tag_stats.top_commands[0].0, "cargo build");
        assert_eq!(tag_stats.top_directories[0].0, "/project");
    }
}
