use crate::models::Entry;
use crate::util::{dirs_home, format_duration_ms, shorten_path, truncate_str};
use crate::{agent_ui, cli, repository, risk, util};

pub fn handle_agent(cmd: cli::AgentCommands) -> Result<(), Box<dyn std::error::Error>> {
    match cmd {
        cli::AgentCommands::Report {
            after,
            before,
            executor,
            format,
            here,
        } => handle_agent_report(
            &after,
            before.as_deref(),
            executor.as_deref(),
            &format,
            here,
        ),
        cli::AgentCommands::Dashboard {
            after,
            executor,
            here,
        } => handle_agent_dashboard(&after, executor.as_deref(), here),
        cli::AgentCommands::Stats {
            days,
            executor,
            text,
        } => {
            if text {
                handle_agent_stats_text(days, executor.as_deref())
            } else {
                handle_agent_stats_tui(days, executor.as_deref())
            }
        }
    }
}

fn handle_agent_report(
    after: &str,
    before: Option<&str>,
    executor: Option<&str>,
    format: &str,
    here: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    let repo = repository::Repository::init()?;

    let after_ms = util::parse_date_input(after, false);
    let before_ms = before.and_then(|d| util::parse_date_input(d, true));

    let cwd_filter = if here {
        Some(std::env::current_dir()?.to_string_lossy().to_string())
    } else {
        None
    };

    // Fetch entries — if no specific executor given, we want all non-human
    // We'll filter out human entries after fetching since the executor filter
    // does substring matching and we need "not human"
    let entries = if let Some(exec) = executor {
        repo.get_replay_entries(
            None,
            after_ms,
            before_ms,
            None,
            None,
            Some(exec),
            cwd_filter.as_deref(),
        )?
    } else {
        // Get all entries, then filter out human
        let all = repo.get_replay_entries(
            None,
            after_ms,
            before_ms,
            None,
            None,
            None,
            cwd_filter.as_deref(),
        )?;
        all.into_iter().filter(Entry::is_agent).collect()
    };

    if entries.is_empty() {
        println!("No agent commands found for the specified period.");
        return Ok(());
    }

    let risk_summary = risk::session_risk(&entries);
    let home = dirs_home();

    match format {
        "json" => print_agent_report_json(&entries, &risk_summary)?,
        "markdown" | "md" => print_agent_report_markdown(&entries, &risk_summary, &home),
        _ => print_agent_report_text(&entries, &risk_summary, &home),
    }

    Ok(())
}

#[allow(clippy::too_many_lines)]
fn print_agent_report_text(entries: &[Entry], risk_summary: &risk::SessionRisk, home: &str) {
    let now = chrono::Local::now();
    let date_str = now.format("%b %d, %Y").to_string();

    // Compute agent breakdown
    let mut agent_counts: std::collections::HashMap<String, usize> =
        std::collections::HashMap::new();
    for e in entries {
        let executor = e.executor.as_deref().unwrap_or("unknown");
        *agent_counts.entry(executor.to_string()).or_default() += 1;
    }
    let mut agents_sorted: Vec<_> = agent_counts.iter().collect();
    agents_sorted.sort_by(|a, b| b.1.cmp(a.1));

    let total = entries.len();
    let success = entries.iter().filter(|e| e.exit_code == Some(0)).count();
    #[allow(clippy::cast_precision_loss)]
    let success_rate = if total > 0 {
        success as f64 / total as f64 * 100.0
    } else {
        0.0
    };

    // Compute time range
    let first_time = entries.first().map_or(0, |e| e.started_at);
    let last_time = entries.last().map_or(0, |e| e.started_at);
    let first_str = format_timestamp_time(first_time);
    let last_str = format_timestamp_time(last_time);

    // Header
    println!();
    println!("\x1b[1m═══════════════════════════════════════════════════════\x1b[0m");
    println!("\x1b[1m  AGENT ACTIVITY REPORT — {date_str}\x1b[0m");
    println!("\x1b[1m═══════════════════════════════════════════════════════\x1b[0m");
    println!();
    println!("  Period:     {first_str} — {last_str}");
    print!("  Agents:     ");
    let agent_strs: Vec<String> = agents_sorted
        .iter()
        .map(|(name, count)| format!("{name} ({count} cmds)"))
        .collect();
    println!("{}", agent_strs.join(", "));
    println!("  Success:    {success}/{total} ({success_rate:.1}%)");
    let risk_parts: Vec<String> = [
        (risk_summary.critical_count, "critical"),
        (risk_summary.high_count, "high"),
        (risk_summary.medium_count, "medium"),
    ]
    .iter()
    .filter(|(c, _)| *c > 0)
    .map(|(c, l)| format!("{c} {l}"))
    .collect();
    if risk_parts.is_empty() {
        println!("  Risk:       \x1b[32m✔ No high-risk commands\x1b[0m");
    } else {
        println!("  Risk:       {}", risk_parts.join(", "));
    }

    // High risk commands
    let high_risk: Vec<_> = entries
        .iter()
        .filter_map(|e| {
            let assessment = risk::assess_risk(&e.command)?;
            if assessment.level >= risk::RiskLevel::High {
                Some((e, assessment))
            } else {
                None
            }
        })
        .collect();

    if !high_risk.is_empty() {
        println!();
        println!("\x1b[1m───────────────────────────────────────────────────────\x1b[0m");
        println!("\x1b[1m  \x1b[38;5;208m⚠ HIGH RISK COMMANDS\x1b[0m");
        println!("\x1b[1m───────────────────────────────────────────────────────\x1b[0m");
        for (entry, assessment) in &high_risk {
            let executor = entry.executor.as_deref().unwrap_or("unknown");
            let time_str = format_timestamp_time(entry.started_at);
            let exit_str = entry
                .exit_code
                .map_or(String::new(), |c| format!("exit {c}"));
            let path = shorten_path(&entry.cwd, home);
            let color = assessment.level.ansi_color();
            println!("  {color}[{executor}]\x1b[0m  {}", entry.command);
            println!("             {path} · {time_str} · {exit_str}");
            println!("             Category: {}", assessment.category);
            println!();
        }
    }

    // Medium risk commands
    let medium_risk: Vec<_> = entries
        .iter()
        .filter_map(|e| {
            let assessment = risk::assess_risk(&e.command)?;
            if assessment.level == risk::RiskLevel::Medium {
                Some((e, assessment))
            } else {
                None
            }
        })
        .collect();

    if !medium_risk.is_empty() {
        println!("\x1b[1m───────────────────────────────────────────────────────\x1b[0m");
        println!("\x1b[1m  \x1b[33m⚡ MEDIUM RISK COMMANDS\x1b[0m");
        println!("\x1b[1m───────────────────────────────────────────────────────\x1b[0m");
        for (entry, _assessment) in medium_risk.iter().take(10) {
            let executor = entry.executor.as_deref().unwrap_or("unknown");
            let time_str = format_timestamp_time(entry.started_at);
            let exit_str = entry
                .exit_code
                .map_or(String::new(), |c| format!("exit {c}"));
            let path = shorten_path(&entry.cwd, home);
            println!("  \x1b[33m[{executor}]\x1b[0m  {}", entry.command);
            println!("             {path} · {time_str} · {exit_str}");
        }
        if medium_risk.len() > 10 {
            println!("  ... and {} more", medium_risk.len() - 10);
        }
        println!();
    }

    // Packages installed
    if !risk_summary.packages_installed.is_empty() {
        println!("\x1b[1m───────────────────────────────────────────────────────\x1b[0m");
        println!("\x1b[1m  📦 PACKAGES INSTALLED\x1b[0m");
        println!("\x1b[1m───────────────────────────────────────────────────────\x1b[0m");
        let mut by_manager: std::collections::HashMap<&str, Vec<String>> =
            std::collections::HashMap::new();
        for pkg in &risk_summary.packages_installed {
            by_manager
                .entry(pkg.manager)
                .or_default()
                .extend(pkg.packages.clone());
        }
        for (manager, packages) in &by_manager {
            println!("  {manager}: {}", packages.join(", "));
        }
        println!();
    }

    // Failed commands
    if !risk_summary.failed_commands.is_empty() {
        println!("\x1b[1m───────────────────────────────────────────────────────\x1b[0m");
        println!(
            "\x1b[1m  \x1b[31m✘ FAILED COMMANDS ({})\x1b[0m",
            risk_summary.failed_commands.len()
        );
        println!("\x1b[1m───────────────────────────────────────────────────────\x1b[0m");
        for fc in risk_summary.failed_commands.iter().take(15) {
            let time_str = format_timestamp_time(fc.timestamp);
            let cmd_trunc = truncate_str(&fc.command, 40, "…");
            println!(
                "  \x1b[31m[{}]\x1b[0m  {:<42} exit {}  {time_str}",
                fc.executor, cmd_trunc, fc.exit_code
            );
        }
        if risk_summary.failed_commands.len() > 15 {
            println!("  ... and {} more", risk_summary.failed_commands.len() - 15);
        }
        println!();
    }

    // Agent breakdown
    if agents_sorted.len() > 1 {
        println!("\x1b[1m───────────────────────────────────────────────────────\x1b[0m");
        println!("\x1b[1m  BREAKDOWN BY AGENT\x1b[0m");
        println!("\x1b[1m───────────────────────────────────────────────────────\x1b[0m");
        let max_count = agents_sorted.first().map_or(1, |(_, c)| **c);
        for (name, count) in &agents_sorted {
            #[allow(clippy::cast_precision_loss)]
            let pct = **count as f64 / total as f64 * 100.0;
            #[allow(
                clippy::cast_precision_loss,
                clippy::cast_possible_truncation,
                clippy::cast_sign_loss
            )]
            let bar_len = (**count as f64 / max_count as f64 * 24.0) as usize;
            let bar: String = "█".repeat(bar_len);
            println!("  {name:<13} \x1b[32m{bar:<24}\x1b[0m  {count:>4}  ({pct:>4.1}%)",);
        }
        println!();
    }

    println!("\x1b[1m═══════════════════════════════════════════════════════\x1b[0m");
    println!();
}

fn print_agent_report_markdown(entries: &[Entry], risk_summary: &risk::SessionRisk, home: &str) {
    let now = chrono::Local::now();
    let date_str = now.format("%b %d, %Y").to_string();

    let mut agent_counts: std::collections::HashMap<String, usize> =
        std::collections::HashMap::new();
    for e in entries {
        let executor = e.executor.as_deref().unwrap_or("unknown");
        *agent_counts.entry(executor.to_string()).or_default() += 1;
    }

    let total = entries.len();
    let success = entries.iter().filter(|e| e.exit_code == Some(0)).count();
    #[allow(clippy::cast_precision_loss)]
    let success_rate = if total > 0 {
        success as f64 / total as f64 * 100.0
    } else {
        0.0
    };

    println!("## Agent Activity Report — {date_str}");
    println!();
    println!("- **Commands:** {total}");
    println!("- **Success rate:** {success_rate:.1}%");
    println!(
        "- **Risk:** {} critical, {} high, {} medium",
        risk_summary.critical_count, risk_summary.high_count, risk_summary.medium_count
    );
    println!();

    // High risk
    let high_risk: Vec<_> = entries
        .iter()
        .filter(|e| risk::risk_level(&e.command) >= risk::RiskLevel::High)
        .collect();
    if !high_risk.is_empty() {
        println!("### High Risk Commands");
        println!();
        println!("| Agent | Command | Dir | Exit | Category |");
        println!("|-------|---------|-----|------|----------|");
        for entry in &high_risk {
            let executor = entry.executor.as_deref().unwrap_or("unknown");
            let assessment = risk::assess_risk(&entry.command);
            let cat = assessment.as_ref().map_or("", |a| a.category);
            let path = shorten_path(&entry.cwd, home);
            let exit = entry
                .exit_code
                .map_or_else(|| String::from("-"), |c| c.to_string());
            let cmd = entry.command.replace('|', "\\|");
            println!("| {executor} | `{cmd}` | {path} | {exit} | {cat} |");
        }
        println!();
    }

    // Packages
    if !risk_summary.packages_installed.is_empty() {
        println!("### Packages Installed");
        println!();
        for pkg in &risk_summary.packages_installed {
            println!("- **{}:** {}", pkg.manager, pkg.packages.join(", "));
        }
        println!();
    }

    // Failures
    if !risk_summary.failed_commands.is_empty() {
        println!("### Failed Commands");
        println!();
        for fc in &risk_summary.failed_commands {
            println!(
                "- `{}` (exit {}, {})",
                fc.command, fc.exit_code, fc.executor
            );
        }
        println!();
    }
}

fn print_agent_report_json(
    entries: &[Entry],
    risk_summary: &risk::SessionRisk,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut report = serde_json::Map::new();

    report.insert(
        "total_commands".into(),
        serde_json::Value::from(entries.len()),
    );
    let success = entries.iter().filter(|e| e.exit_code == Some(0)).count();
    report.insert("success_count".into(), serde_json::Value::from(success));
    report.insert(
        "critical_risk_count".into(),
        serde_json::Value::from(risk_summary.critical_count),
    );
    report.insert(
        "high_risk_count".into(),
        serde_json::Value::from(risk_summary.high_count),
    );
    report.insert(
        "medium_risk_count".into(),
        serde_json::Value::from(risk_summary.medium_count),
    );

    // Package installs
    let packages: Vec<serde_json::Value> = risk_summary
        .packages_installed
        .iter()
        .map(|p| {
            serde_json::json!({
                "manager": p.manager,
                "packages": p.packages,
            })
        })
        .collect();
    report.insert(
        "packages_installed".into(),
        serde_json::Value::from(packages),
    );

    // Entries with risk
    let entries_json: Vec<serde_json::Value> = entries
        .iter()
        .map(|e| {
            let assessment = risk::assess_risk(&e.command);
            serde_json::json!({
                "command": e.command,
                "cwd": e.cwd,
                "exit_code": e.exit_code,
                "executor_type": e.executor_type,
                "executor": e.executor,
                "started_at": e.started_at,
                "duration_ms": e.duration_ms,
                "risk_level": assessment.as_ref().map_or("none", |a| a.level.label()),
                "risk_category": assessment.as_ref().map(|a| a.category),
            })
        })
        .collect();
    report.insert("entries".into(), serde_json::Value::from(entries_json));

    println!(
        "{}",
        serde_json::to_string_pretty(&serde_json::Value::Object(report))?
    );
    Ok(())
}

fn handle_agent_dashboard(
    after: &str,
    executor: Option<&str>,
    here: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    let repo = repository::Repository::init()?;

    let after_ms = util::parse_date_input(after, false);
    let cwd_filter = if here {
        Some(std::env::current_dir()?.to_string_lossy().to_string())
    } else {
        None
    };

    crossterm::terminal::enable_raw_mode()?;
    let mut stdout = std::io::stdout();
    crossterm::execute!(stdout, crossterm::terminal::EnterAlternateScreen)?;
    let backend = ratatui::backend::CrosstermBackend::new(stdout);
    let mut terminal = ratatui::Terminal::new(backend)?;

    let res = agent_ui::run_agent_ui(
        &mut terminal,
        &repo,
        after_ms,
        executor,
        cwd_filter.as_deref(),
    );

    crossterm::terminal::disable_raw_mode()?;
    crossterm::execute!(
        terminal.backend_mut(),
        crossterm::terminal::LeaveAlternateScreen
    )?;
    terminal.show_cursor()?;

    if let Err(e) = res {
        eprintln!("Error in agent UI: {e}");
    }
    Ok(())
}

fn handle_agent_stats_tui(
    days: usize,
    executor: Option<&str>,
) -> Result<(), Box<dyn std::error::Error>> {
    let repo = repository::Repository::init()?;

    crossterm::terminal::enable_raw_mode()?;
    let mut stdout = std::io::stdout();
    crossterm::execute!(stdout, crossterm::terminal::EnterAlternateScreen)?;
    let backend = ratatui::backend::CrosstermBackend::new(stdout);
    let mut terminal = ratatui::Terminal::new(backend)?;

    let res = agent_ui::run_agent_stats_ui(&mut terminal, &repo, days, executor);

    crossterm::terminal::disable_raw_mode()?;
    crossterm::execute!(
        terminal.backend_mut(),
        crossterm::terminal::LeaveAlternateScreen
    )?;
    terminal.show_cursor()?;

    if let Err(e) = res {
        eprintln!("Error in agent stats UI: {e}");
    }
    Ok(())
}

#[allow(clippy::too_many_lines)]
fn handle_agent_stats_text(
    days: usize,
    executor: Option<&str>,
) -> Result<(), Box<dyn std::error::Error>> {
    let repo = repository::Repository::init()?;

    let now = chrono::Utc::now().timestamp_millis();
    let days_ms = i64::try_from(days)
        .unwrap_or(i64::MAX)
        .saturating_mul(86_400_000);
    let after_ms = Some(now.saturating_sub(days_ms));

    // Fetch all entries in the period
    let all_entries = repo.get_replay_entries(None, after_ms, None, None, None, executor, None)?;

    // Filter to agent entries only
    let entries: Vec<_> = if executor.is_some() {
        all_entries
    } else {
        all_entries.into_iter().filter(Entry::is_agent).collect()
    };

    if entries.is_empty() {
        println!("No agent commands found in the last {days} days.");
        return Ok(());
    }

    // Group by executor
    let mut by_agent: std::collections::HashMap<String, Vec<&Entry>> =
        std::collections::HashMap::new();
    for e in &entries {
        let name = e.executor.as_deref().unwrap_or("unknown");
        by_agent.entry(name.to_string()).or_default().push(e);
    }
    let mut agents: Vec<_> = by_agent.keys().cloned().collect();
    agents.sort();

    let home = dirs_home();

    println!();
    println!("\x1b[1mAgent Analytics (last {days} days)\x1b[0m");
    println!("\x1b[1m═══════════════════════════════════════════\x1b[0m");
    println!();

    for agent in &agents {
        let cmds = &by_agent[agent];
        let total = cmds.len();
        let success = cmds.iter().filter(|e| e.exit_code == Some(0)).count();
        #[allow(clippy::cast_precision_loss)]
        let rate = if total > 0 {
            success as f64 / total as f64 * 100.0
        } else {
            0.0
        };
        #[allow(clippy::cast_precision_loss)]
        let avg_dur = if total > 0 {
            cmds.iter().map(|e| e.duration_ms).sum::<i64>() as f64 / total as f64
        } else {
            0.0
        };

        let risk_entries: Vec<Entry> = cmds.iter().map(|e| (*e).clone()).collect();
        let risk_summary = risk::session_risk(&risk_entries);

        println!("  \x1b[1m{agent}\x1b[0m");
        println!("  {}", "─".repeat(agent.len() + 2));
        println!("  Commands:     {total}");
        println!("  Success:      {rate:.1}%");
        #[allow(clippy::cast_possible_truncation)]
        let avg_dur_i64 = avg_dur as i64;
        println!("  Avg duration: {}", format_duration_ms(avg_dur_i64));
        println!(
            "  High risk:    {}",
            risk_summary.critical_count + risk_summary.high_count
        );
        if !risk_summary.packages_installed.is_empty() {
            let pkg_count: usize = risk_summary
                .packages_installed
                .iter()
                .map(|p| p.packages.len())
                .sum();
            println!("  Packages:     {pkg_count}");
        }
        println!();

        // Top directories
        let mut dir_counts: std::collections::HashMap<String, usize> =
            std::collections::HashMap::new();
        for e in cmds {
            *dir_counts.entry(e.cwd.clone()).or_default() += 1;
        }
        let mut top_dirs: Vec<_> = dir_counts.into_iter().collect();
        top_dirs.sort_by(|a, b| b.1.cmp(&a.1));

        println!("  Top Directories ({agent})");
        println!("  {}", "─".repeat(30));
        for (i, (dir, count)) in top_dirs.iter().take(10).enumerate() {
            let short = shorten_path(dir, &home);
            println!("  {}. {:<30} ({count})", i + 1, short);
        }
        println!();

        // High risk commands (recent first, max 20)
        let mut high_risk_cmds: Vec<_> = cmds
            .iter()
            .filter_map(|e| {
                risk::assess_risk(&e.command).and_then(|a| {
                    if a.level >= risk::RiskLevel::High {
                        Some((e, a))
                    } else {
                        None
                    }
                })
            })
            .collect();
        high_risk_cmds.sort_by(|a, b| b.0.started_at.cmp(&a.0.started_at));
        high_risk_cmds.truncate(20);

        if !high_risk_cmds.is_empty() {
            println!("  \x1b[33mHigh Risk Commands ({agent})\x1b[0m");
            println!("  {}", "─".repeat(50));
            for (e, a) in &high_risk_cmds {
                let color = a.level.ansi_color();
                let path = shorten_path(&e.cwd, &home);
                let time = format_timestamp_time(e.started_at);
                let status = match e.exit_code {
                    Some(0) => "\x1b[32mok\x1b[0m".to_string(),
                    Some(c) => format!("\x1b[31mE{c}\x1b[0m"),
                    None => "??".to_string(),
                };
                println!(
                    "  {color}{:<9}\x1b[0m {}  \x1b[90m{path}  {time}  {status}\x1b[0m",
                    format!("{}", a.level),
                    e.command
                );
            }
            println!();
        }
    }

    // Overall risk summary
    let all_risk = entries.clone();
    let overall = risk::session_risk(&all_risk);
    if overall.critical_count + overall.high_count > 0 {
        println!("\x1b[1m  High Risk Summary\x1b[0m");
        println!("  {}", "─".repeat(20));
        for e in &entries {
            let assessment = risk::assess_risk(&e.command);
            if let Some(a) = &assessment {
                if a.level >= risk::RiskLevel::High {
                    let executor = e.executor.as_deref().unwrap_or("?");
                    let path = shorten_path(&e.cwd, &home);
                    let color = a.level.ansi_color();
                    println!("  {color}[{executor}]\x1b[0m  {}  ({path})", e.command);
                }
            }
        }
        println!();
    }

    println!("\x1b[1m═══════════════════════════════════════════\x1b[0m");
    println!();
    Ok(())
}

fn format_timestamp_time(ms: i64) -> String {
    use chrono::TimeZone;
    let ms_val = if ms > 1_000_000_000_000_000 {
        ms / 1000
    } else {
        ms
    };
    chrono::Local
        .timestamp_millis_opt(ms_val)
        .single()
        .map_or_else(|| "??:??".into(), |dt| dt.format("%H:%M").to_string())
}
