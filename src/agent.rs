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
            &crate::repository::ReplayFilter {
                after: after_ms,
                before: before_ms,
                executor: Some(exec),
                cwd: cwd_filter.as_deref(),
                ..Default::default()
            },
        )?
    } else {
        // Get all entries, then filter out human
        let all = repo.get_replay_entries(
            None,
            &crate::repository::ReplayFilter {
                after: after_ms,
                before: before_ms,
                cwd: cwd_filter.as_deref(),
                ..Default::default()
            },
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

fn print_agent_report_text(entries: &[Entry], risk_summary: &risk::SessionRisk, home: &str) {
    let c = util::color_enabled();
    let mut agent_counts: std::collections::HashMap<String, usize> =
        std::collections::HashMap::new();
    for e in entries {
        let executor = e.executor.as_deref().unwrap_or("unknown");
        *agent_counts.entry(executor.to_string()).or_default() += 1;
    }
    let mut agents_sorted: Vec<_> = agent_counts.iter().collect();
    agents_sorted.sort_by(|a, b| b.1.cmp(a.1));

    let total = entries.len();

    print_report_header(entries, &agents_sorted, total, risk_summary, c);
    print_report_high_risk(entries, home, c);
    print_report_medium_risk(entries, home, c);
    print_report_packages(risk_summary, c);
    print_report_failures(risk_summary, c);
    print_report_agent_breakdown(&agents_sorted, total, c);

    let (b, r) = if c { ("\x1b[1m", "\x1b[0m") } else { ("", "") };
    println!("{b}═══════════════════════════════════════════════════════{r}");
    println!();
}

fn print_report_header(
    entries: &[Entry],
    agents_sorted: &[(&String, &usize)],
    total: usize,
    risk_summary: &risk::SessionRisk,
    c: bool,
) {
    let now = chrono::Local::now();
    let date_str = now.format("%b %d, %Y").to_string();

    let success = entries.iter().filter(|e| e.exit_code == Some(0)).count();
    #[allow(clippy::cast_precision_loss)]
    let success_rate = if total > 0 {
        success as f64 / total as f64 * 100.0
    } else {
        0.0
    };

    let first_str = format_timestamp_time(entries.first().map_or(0, |e| e.started_at));
    let last_str = format_timestamp_time(entries.last().map_or(0, |e| e.started_at));

    let (b, r) = if c { ("\x1b[1m", "\x1b[0m") } else { ("", "") };
    println!();
    println!("{b}═══════════════════════════════════════════════════════{r}");
    println!("{b}  AGENT ACTIVITY REPORT — {date_str}{r}");
    println!("{b}═══════════════════════════════════════════════════════{r}");
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
        if c {
            println!("  Risk:       \x1b[32m✔ No high-risk commands\x1b[0m");
        } else {
            println!("  Risk:       ✔ No high-risk commands");
        }
    } else {
        println!("  Risk:       {}", risk_parts.join(", "));
    }
}

fn print_report_high_risk(entries: &[Entry], home: &str, c: bool) {
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
        let (b, r) = if c { ("\x1b[1m", "\x1b[0m") } else { ("", "") };
        let orange = if c { "\x1b[38;5;208m" } else { "" };
        println!();
        println!("{b}───────────────────────────────────────────────────────{r}");
        println!("{b}  {orange}⚠ HIGH RISK COMMANDS{r}");
        println!("{b}───────────────────────────────────────────────────────{r}");
        for (entry, assessment) in &high_risk {
            let executor = entry.executor.as_deref().unwrap_or("unknown");
            let time_str = format_timestamp_time(entry.started_at);
            let exit_str = entry
                .exit_code
                .map_or(String::new(), |c| format!("exit {c}"));
            let path = shorten_path(&entry.cwd, home);
            let color = if c { assessment.level.ansi_color() } else { "" };
            println!("  {color}[{executor}]{r}  {}", entry.command);
            println!("             {path} · {time_str} · {exit_str}");
            println!("             Category: {}", assessment.category);
            println!();
        }
    }
}

fn print_report_medium_risk(entries: &[Entry], home: &str, c: bool) {
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
        let (b, r) = if c { ("\x1b[1m", "\x1b[0m") } else { ("", "") };
        let yellow = if c { "\x1b[33m" } else { "" };
        println!("{b}───────────────────────────────────────────────────────{r}");
        println!("{b}  {yellow}⚡ MEDIUM RISK COMMANDS{r}");
        println!("{b}───────────────────────────────────────────────────────{r}");
        for (entry, _assessment) in medium_risk.iter().take(10) {
            let executor = entry.executor.as_deref().unwrap_or("unknown");
            let time_str = format_timestamp_time(entry.started_at);
            let exit_str = entry
                .exit_code
                .map_or(String::new(), |c| format!("exit {c}"));
            let path = shorten_path(&entry.cwd, home);
            println!("  {yellow}[{executor}]{r}  {}", entry.command);
            println!("             {path} · {time_str} · {exit_str}");
        }
        if medium_risk.len() > 10 {
            println!("  ... and {} more", medium_risk.len() - 10);
        }
        println!();
    }
}

fn print_report_packages(risk_summary: &risk::SessionRisk, c: bool) {
    if !risk_summary.packages_installed.is_empty() {
        let (b, r) = if c { ("\x1b[1m", "\x1b[0m") } else { ("", "") };
        println!("{b}───────────────────────────────────────────────────────{r}");
        println!("{b}  📦 PACKAGES INSTALLED{r}");
        println!("{b}───────────────────────────────────────────────────────{r}");
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
}

fn print_report_failures(risk_summary: &risk::SessionRisk, c: bool) {
    if !risk_summary.failed_commands.is_empty() {
        let (b, r) = if c { ("\x1b[1m", "\x1b[0m") } else { ("", "") };
        let red = if c { "\x1b[31m" } else { "" };
        println!("{b}───────────────────────────────────────────────────────{r}");
        println!(
            "{b}  {red}✘ FAILED COMMANDS ({}){r}",
            risk_summary.failed_commands.len()
        );
        println!("{b}───────────────────────────────────────────────────────{r}");
        for fc in risk_summary.failed_commands.iter().take(15) {
            let time_str = format_timestamp_time(fc.timestamp);
            let cmd_trunc = truncate_str(&fc.command, 40, "…");
            println!(
                "  {red}[{}]{r}  {:<42} exit {}  {time_str}",
                fc.executor, cmd_trunc, fc.exit_code
            );
        }
        if risk_summary.failed_commands.len() > 15 {
            println!("  ... and {} more", risk_summary.failed_commands.len() - 15);
        }
        println!();
    }
}

fn print_report_agent_breakdown(agents_sorted: &[(&String, &usize)], total: usize, c: bool) {
    if agents_sorted.len() > 1 {
        let (b, r) = if c { ("\x1b[1m", "\x1b[0m") } else { ("", "") };
        let green = if c { "\x1b[32m" } else { "" };
        println!("{b}───────────────────────────────────────────────────────{r}");
        println!("{b}  BREAKDOWN BY AGENT{r}");
        println!("{b}───────────────────────────────────────────────────────{r}");
        let max_count = agents_sorted.first().map_or(1, |(_, c)| **c);
        for (name, count) in agents_sorted {
            #[allow(clippy::cast_precision_loss)]
            let pct = **count as f64 / total as f64 * 100.0;
            #[allow(
                clippy::cast_precision_loss,
                clippy::cast_possible_truncation,
                clippy::cast_sign_loss
            )]
            let bar_len = (**count as f64 / max_count as f64 * 24.0) as usize;
            let bar: String = "█".repeat(bar_len);
            println!("  {name:<13} {green}{bar:<24}{r}  {count:>4}  ({pct:>4.1}%)",);
        }
        println!();
    }
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
    let json = build_agent_report_json(entries, risk_summary);
    println!("{}", serde_json::to_string_pretty(&json)?);
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

    let mut guard = util::TerminalGuard::new()?;
    let res = agent_ui::run_agent_ui(
        guard.terminal(),
        &repo,
        after_ms,
        executor,
        cwd_filter.as_deref(),
    );
    drop(guard);

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

    let mut guard = util::TerminalGuard::new()?;
    let res = agent_ui::run_agent_stats_ui(guard.terminal(), &repo, days, executor);
    drop(guard);

    if let Err(e) = res {
        eprintln!("Error in agent stats UI: {e}");
    }
    Ok(())
}

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

    let all_entries = repo.get_replay_entries(
        None,
        &crate::repository::ReplayFilter {
            after: after_ms,
            executor,
            ..Default::default()
        },
    )?;

    let entries: Vec<_> = if executor.is_some() {
        all_entries
    } else {
        all_entries.into_iter().filter(Entry::is_agent).collect()
    };

    if entries.is_empty() {
        println!("No agent commands found in the last {days} days.");
        return Ok(());
    }

    let mut by_agent: std::collections::HashMap<String, Vec<&Entry>> =
        std::collections::HashMap::new();
    for e in &entries {
        let name = e.executor.as_deref().unwrap_or("unknown");
        by_agent.entry(name.to_string()).or_default().push(e);
    }
    let mut agents: Vec<_> = by_agent.keys().cloned().collect();
    agents.sort();

    let home = dirs_home();
    let c = util::color_enabled();

    let (b, r) = if c { ("\x1b[1m", "\x1b[0m") } else { ("", "") };
    println!();
    println!("{b}Agent Analytics (last {days} days){r}");
    println!("{b}═══════════════════════════════════════════{r}");
    println!();

    for agent in &agents {
        let cmds = &by_agent[agent];
        print_stats_agent_summary(agent, cmds, c);
        print_stats_top_dirs(agent, cmds, &home);
        print_stats_high_risk_cmds(agent, cmds, &home, c);
    }

    print_stats_overall_risk(&entries, &home, c);

    println!("{b}═══════════════════════════════════════════{r}");
    println!();
    Ok(())
}

fn print_stats_agent_summary(agent: &str, cmds: &[&Entry], c: bool) {
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
        cmds.iter()
            .fold(0i64, |acc, e| acc.saturating_add(e.duration_ms)) as f64
            / total as f64
    } else {
        0.0
    };

    let risk_entries: Vec<Entry> = cmds.iter().map(|e| (*e).clone()).collect();
    let risk_summary = risk::session_risk(&risk_entries);

    let (b, r) = if c { ("\x1b[1m", "\x1b[0m") } else { ("", "") };
    println!("  {b}{agent}{r}");
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
}

fn print_stats_top_dirs(agent: &str, cmds: &[&Entry], home: &str) {
    let mut dir_counts: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
    for e in cmds {
        *dir_counts.entry(e.cwd.clone()).or_default() += 1;
    }
    let mut top_dirs: Vec<_> = dir_counts.into_iter().collect();
    top_dirs.sort_by(|a, b| b.1.cmp(&a.1));

    println!("  Top Directories ({agent})");
    println!("  {}", "─".repeat(30));
    for (i, (dir, count)) in top_dirs.iter().take(10).enumerate() {
        let short = shorten_path(dir, home);
        println!("  {}. {:<30} ({count})", i + 1, short);
    }
    println!();
}

fn print_stats_high_risk_cmds(agent: &str, cmds: &[&Entry], home: &str, c: bool) {
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
        let r = if c { "\x1b[0m" } else { "" };
        let yellow = if c { "\x1b[33m" } else { "" };
        let dim = if c { "\x1b[90m" } else { "" };
        println!("  {yellow}High Risk Commands ({agent}){r}");
        println!("  {}", "─".repeat(50));
        for (e, a) in &high_risk_cmds {
            let color = if c { a.level.ansi_color() } else { "" };
            let path = shorten_path(&e.cwd, home);
            let time = format_timestamp_time(e.started_at);
            let status = if c {
                match e.exit_code {
                    Some(0) => "\x1b[32mok\x1b[0m".to_string(),
                    Some(code) => format!("\x1b[31mE{code}\x1b[0m"),
                    None => "??".to_string(),
                }
            } else {
                match e.exit_code {
                    Some(0) => "ok".to_string(),
                    Some(code) => format!("E{code}"),
                    None => "??".to_string(),
                }
            };
            println!(
                "  {color}{:<9}{r} {}  {dim}{path}  {time}  {status}{r}",
                format!("{}", a.level),
                e.command
            );
        }
        println!();
    }
}

fn print_stats_overall_risk(entries: &[Entry], home: &str, c: bool) {
    let overall = risk::session_risk(entries);
    if overall.critical_count + overall.high_count > 0 {
        let (b, r) = if c { ("\x1b[1m", "\x1b[0m") } else { ("", "") };
        println!("{b}  High Risk Summary{r}");
        println!("  {}", "─".repeat(20));
        for e in entries {
            let assessment = risk::assess_risk(&e.command);
            if let Some(a) = &assessment {
                if a.level >= risk::RiskLevel::High {
                    let executor = e.executor.as_deref().unwrap_or("?");
                    let path = shorten_path(&e.cwd, home);
                    let color = if c { a.level.ansi_color() } else { "" };
                    println!("  {color}[{executor}]{r}  {}  ({path})", e.command);
                }
            }
        }
        println!();
    }
}

fn format_timestamp_time(ms: i64) -> String {
    use chrono::TimeZone;
    let ms_val = crate::util::normalize_display_ms(ms);
    chrono::Local
        .timestamp_millis_opt(ms_val)
        .single()
        .map_or_else(|| "??:??".into(), |dt| dt.format("%H:%M").to_string())
}

/// Build the agent-report JSON value without printing it.
/// Extracted so that `print_agent_report_json` and tests can share logic.
fn build_agent_report_json(
    entries: &[Entry],
    risk_summary: &risk::SessionRisk,
) -> serde_json::Value {
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

    serde_json::Value::Object(report)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Helper: build an `Entry` with the given executor_type and executor,
    /// using sensible defaults for everything else.
    fn make_entry(command: &str, executor_type: Option<&str>, executor: Option<&str>) -> Entry {
        let mut e = Entry::new(
            "sess-1".to_string(),
            command.to_string(),
            "/tmp".to_string(),
            Some(0),
            1_700_000_000_000, // ~2023-11-14 in ms
            1_700_000_001_000,
        );
        e.executor_type = executor_type.map(String::from);
        e.executor = executor.map(String::from);
        e
    }

    // ── format_timestamp_time ───────────────────────────────────────

    #[test]
    fn format_timestamp_time_normal_ms() {
        // A known millisecond timestamp should produce a non-error HH:MM string.
        let result = format_timestamp_time(1_700_000_000_000);
        // Cannot hard-code the hour because the local timezone varies,
        // but it must NOT be the error sentinel.
        assert_ne!(result, "??:??");
        // Should match HH:MM pattern.
        assert_eq!(result.len(), 5);
        assert_eq!(result.as_bytes()[2], b':');
    }

    #[test]
    fn format_timestamp_time_microsecond_normalization() {
        // Timestamp > 1e15 is treated as microseconds and divided by 1000.
        // 1_700_000_000_000_000 µs == 1_700_000_000_000 ms (same instant).
        let from_us = format_timestamp_time(1_700_000_000_000_000);
        let from_ms = format_timestamp_time(1_700_000_000_000);
        assert_eq!(from_us, from_ms);
    }

    #[test]
    fn format_timestamp_time_invalid_value() {
        // A value that chrono cannot represent should return the sentinel.
        let result = format_timestamp_time(i64::MIN);
        assert_eq!(result, "??:??");
    }

    // ── build_agent_report_json (same logic as print_agent_report_json) ─

    #[test]
    fn agent_report_json_has_expected_fields() {
        let entries = vec![
            make_entry("ls", Some("claude"), Some("claude")),
            make_entry("rm -rf /important", Some("claude"), Some("claude")),
            make_entry("cargo build", Some("copilot"), Some("copilot")),
        ];
        let risk_summary = risk::session_risk(&entries);

        let json = build_agent_report_json(&entries, &risk_summary);

        // Round-trip through serialization to prove the output is valid JSON.
        let serialized = serde_json::to_string_pretty(&json).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&serialized)
            .expect("build_agent_report_json should produce valid JSON");

        let obj = parsed.as_object().expect("top-level should be an object");
        assert_eq!(obj["total_commands"], 3);
        assert_eq!(obj["success_count"], 3);
        assert!(obj.contains_key("critical_risk_count"));
        assert!(obj.contains_key("high_risk_count"));
        assert!(obj.contains_key("medium_risk_count"));
        assert!(obj.contains_key("packages_installed"));
        assert!(obj.contains_key("entries"));

        let arr = obj["entries"].as_array().unwrap();
        assert_eq!(arr.len(), 3);

        // Each entry object should carry the expected per-command fields.
        for item in arr {
            let m = item.as_object().unwrap();
            assert!(m.contains_key("command"));
            assert!(m.contains_key("risk_level"));
            assert!(m.contains_key("risk_category"));
            assert!(m.contains_key("executor"));
            assert!(m.contains_key("duration_ms"));
        }
    }

    #[test]
    fn agent_report_json_risk_levels_populated() {
        // "rm -rf /" is critical risk; make sure the entry reflects that.
        let entries = vec![make_entry(
            "rm -rf /important",
            Some("claude"),
            Some("claude"),
        )];
        let risk_summary = risk::session_risk(&entries);
        let json = build_agent_report_json(&entries, &risk_summary);
        let obj = json.as_object().unwrap();

        // The critical_risk_count should be at least 1.
        assert!(obj["critical_risk_count"].as_u64().unwrap() >= 1);

        let entry_obj = obj["entries"].as_array().unwrap()[0].as_object().unwrap();
        assert_eq!(entry_obj["risk_level"], "critical");
        assert_eq!(entry_obj["risk_category"], "destructive");
    }

    // ── agent filtering (Entry::is_agent gate) ──────────────────────

    #[test]
    fn agent_filter_excludes_human_and_unknown() {
        let entries = vec![
            make_entry("echo a", Some("claude"), Some("claude")),
            make_entry("echo b", Some("human"), None),
            make_entry("echo c", Some("unknown"), None),
            make_entry("echo d", Some("copilot"), Some("copilot")),
            make_entry("echo e", None, None), // executor_type is None → is_agent false
        ];

        let agent_only: Vec<_> = entries.into_iter().filter(Entry::is_agent).collect();

        assert_eq!(agent_only.len(), 2);
        assert_eq!(agent_only[0].command, "echo a");
        assert_eq!(agent_only[1].command, "echo d");
    }

    // ── days filter: saturating_mul ─────────────────────────────────

    #[test]
    fn days_ms_saturating_mul_does_not_overflow() {
        // Mirrors the calculation in handle_agent_stats_text.
        let days: usize = usize::MAX;
        let days_ms = i64::try_from(days)
            .unwrap_or(i64::MAX)
            .saturating_mul(86_400_000);
        assert_eq!(days_ms, i64::MAX);
    }

    #[test]
    fn days_ms_normal_value() {
        let days: usize = 7;
        let days_ms = i64::try_from(days)
            .unwrap_or(i64::MAX)
            .saturating_mul(86_400_000);
        assert_eq!(days_ms, 7 * 86_400_000);
    }
}
