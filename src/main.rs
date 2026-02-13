use clap::Parser;
use std::process;

mod agent;
mod agent_ui;
mod cli;
mod config;
mod db;
mod hooks;
mod import_export;
mod integrations;
mod models;
mod repository;
mod risk;
mod search;
mod settings_ui;
mod stats_ui;
mod suggest;
mod suggest_ui;
mod theme;
mod update;
mod util;

use cli::{Cli, Commands};
use models::{Entry, Session};
use repository::Repository;
use util::{dirs_home, format_count, format_duration_ms, shorten_path};

fn main() {
    let cli = Cli::parse();

    if let Err(e) = run(cli) {
        eprintln!("Error: {e}");
        process::exit(1);
    }
}

#[allow(clippy::too_many_lines)]
fn run(cli: Cli) -> Result<(), Box<dyn std::error::Error>> {
    match cli.command {
        Commands::Enable => {
            let mut cfg = config::load_config()?;
            cfg.enabled = true;
            config::save_config(&cfg)?;
            println!("✓ History recording enabled globally");
            Ok(())
        }

        Commands::Disable => {
            let mut cfg = config::load_config()?;
            cfg.enabled = false;
            config::save_config(&cfg)?;
            println!("✓ History recording disabled globally");
            Ok(())
        }

        Commands::Pause => {
            // Output shell export command for eval
            println!("export SUVADU_PAUSED=1");
            Ok(())
        }

        Commands::Add {
            session_id,
            command,
            cwd,
            exit_code,
            started_at,
            ended_at,
            executor_type,
            executor,
        } => handle_add(
            &session_id,
            command,
            cwd,
            exit_code,
            started_at,
            ended_at,
            executor_type,
            executor,
        ),

        Commands::Init { target } => match target.as_str() {
            "zsh" => {
                let config = config::load_config().unwrap_or_default();
                println!("{}", hooks::get_zsh_hook(&config)?);
                Ok(())
            }
            "bash" => {
                let config = config::load_config().unwrap_or_default();
                println!("{}", hooks::get_bash_hook(&config)?);
                Ok(())
            }
            "claude-code" => integrations::handle_init_claude_code(),
            "cursor" => integrations::handle_init_ide("Cursor", "Suvadu detects Cursor via $CURSOR_INJECTION and\n$CURSOR_TRACE_ID environment variables.", "cursor"),
            "antigravity" => integrations::handle_init_ide("Antigravity", "Suvadu detects Antigravity via the $ANTIGRAVITY_AGENT\nenvironment variable.", "antigravity"),
            _ => {
                eprintln!(
                    "Unsupported target: {target}. Use 'zsh', 'bash', 'claude-code', 'cursor', or 'antigravity'."
                );
                process::exit(1);
            }
        },

        Commands::HookClaudeCode => integrations::handle_hook_claude_code(),
        Commands::HookClaudePrompt => integrations::handle_hook_claude_prompt(),

        Commands::Search {
            query,
            unique,
            after,
            before,
            tag,
            exit_code,
            executor,
            here,
        } => handle_search(
            query.as_ref(),
            unique,
            after.as_deref(),
            before.as_deref(),
            tag.as_deref(),
            exit_code,
            executor.as_deref(),
            here,
        ),

        Commands::Get {
            query,
            offset,
            prefix,
            cwd,
        } => handle_get(&query, offset, prefix, cwd.as_deref()),

        Commands::Settings => handle_settings(),

        Commands::Status => handle_status(),

        Commands::Tag(cmd) => handle_tag(cmd),

        Commands::Bookmark(cmd) => handle_bookmark(cmd),

        Commands::Note {
            entry_id,
            content,
            delete,
        } => handle_note(entry_id, content, delete),

        Commands::Delete {
            pattern,
            regex,
            dry_run,
            before,
        } => handle_delete(&pattern, regex, dry_run, before.as_deref()),

        Commands::Uninstall => handle_uninstall(),

        Commands::Version => {
            println!(
                "suvadu v{} ({})",
                env!("CARGO_PKG_VERSION"),
                env!("BUILD_DATE")
            );
            Ok(())
        }

        Commands::Man => cli::generate_man_page(),

        Commands::Completions { shell } => {
            cli::generate_completions(shell);
            Ok(())
        }

        Commands::Update => update::handle_update(),

        Commands::Wrap {
            command,
            executor_type,
            executor,
        } => handle_wrap(&command, &executor_type, &executor),

        Commands::Export {
            format,
            after,
            before,
        } => import_export::handle_export(&format, after.as_deref(), before.as_deref()),

        Commands::Import {
            file,
            from,
            dry_run,
        } => match from.as_str() {
            "jsonl" => import_export::handle_import(&file, dry_run),
            "zsh-history" => import_export::handle_import_zsh_history(&file, dry_run),
            _ => {
                eprintln!("Unknown import format: {from}. Use 'jsonl' or 'zsh-history'.");
                process::exit(1);
            }
        },

        Commands::Stats { days, top, text } => {
            if text {
                handle_stats_text(days, top)
            } else {
                handle_stats_tui(days, top)
            }
        }

        Commands::SuggestAliases {
            min_count,
            min_length,
            days,
            top,
            text,
        } => {
            if text {
                suggest::handle_suggest_aliases_text(min_count, min_length, days, top)
            } else {
                suggest::handle_suggest_aliases_tui(min_count, min_length, days, top)
            }
        }

        Commands::Replay {
            session,
            after,
            before,
            tag,
            exit_code,
            executor,
            here,
            cwd,
        } => handle_replay(
            session.as_deref(),
            after.as_deref(),
            before.as_deref(),
            tag.as_deref(),
            exit_code,
            executor.as_deref(),
            here,
            cwd.as_deref(),
        ),

        Commands::Agent(cmd) => agent::handle_agent(cmd),
    }
}

fn handle_wrap(
    command: &[String],
    executor_type: &str,
    executor: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    if command.is_empty() {
        eprintln!("Error: No command provided to wrap.");
        process::exit(1);
    }

    let cmd_str = command.join(" ");
    let cwd = std::env::current_dir()
        .map_or_else(|_| ".".to_string(), |p| p.to_string_lossy().to_string());

    let session_id =
        std::env::var("SUVADU_SESSION_ID").unwrap_or_else(|_| uuid::Uuid::new_v4().to_string());

    let started_at = chrono::Utc::now().timestamp_millis();

    // Execute the command
    let status = std::process::Command::new("sh")
        .arg("-c")
        .arg(&cmd_str)
        .status();

    let ended_at = chrono::Utc::now().timestamp_millis();

    let exit_code = match &status {
        Ok(s) => s.code().unwrap_or(1),
        Err(_) => 127,
    };

    // Record in history
    let _ = handle_add(
        &session_id,
        cmd_str,
        cwd,
        Some(exit_code),
        started_at,
        ended_at,
        Some(executor_type.to_string()),
        Some(executor.to_string()),
    );

    // Exit with the command's exit code
    process::exit(exit_code);
}

#[allow(
    clippy::too_many_arguments,
    clippy::too_many_lines,
    clippy::cast_precision_loss,
    clippy::cast_possible_wrap
)]
fn handle_replay(
    session: Option<&str>,
    after: Option<&str>,
    before: Option<&str>,
    tag: Option<&str>,
    exit_code: Option<i32>,
    executor: Option<&str>,
    here: bool,
    cwd: Option<&str>,
) -> Result<(), Box<dyn std::error::Error>> {
    let db_path = db::get_db_path()?;
    let conn = db::init_db(&db_path)?;
    let repo = repository::Repository::new(conn);

    // Resolve tag name → id
    let tag_id = if let Some(tname) = tag {
        let tags = repo.get_tags()?;
        let tname_lower = tname.to_lowercase();
        tags.iter().find(|t| t.name == tname_lower).map(|t| t.id)
    } else {
        None
    };

    // Resolve --here flag
    let cwd_filter = if here {
        Some(std::env::current_dir()?.to_string_lossy().to_string())
    } else {
        cwd.map(String::from)
    };

    // Parse date filters
    let after_ms = after.and_then(|d| util::parse_date_input(d, false));
    let before_ms = before.and_then(|d| util::parse_date_input(d, true));

    // Determine session filter: explicit --session, or fallback to env var, or fallback to last 24h
    let session_filter;
    let mut effective_after = after_ms;
    let label: String;

    if let Some(sid) = session {
        session_filter = Some(sid.to_string());
        label = format!("session {}", &sid[..sid.len().min(8)]);
    } else if after_ms.is_some() || before_ms.is_some() || tag.is_some() || here || cwd.is_some() {
        // User specified time/filter flags — don't scope to session
        session_filter = None;
        let parts: Vec<String> = [
            after.map(|d| format!("after {d}")),
            before.map(|d| format!("before {d}")),
            tag.map(|t| format!("tag:{t}")),
            if here {
                Some("current dir".into())
            } else {
                cwd.map(|d| format!("dir:{d}"))
            },
        ]
        .into_iter()
        .flatten()
        .collect();
        label = if parts.is_empty() {
            "all time".into()
        } else {
            parts.join(", ")
        };
    } else if let Ok(sid) = std::env::var("SUVADU_SESSION_ID") {
        session_filter = Some(sid.clone());
        label = format!("current session ({})", &sid[..sid.len().min(8)]);
    } else {
        // No session env, no flags → last 24h
        session_filter = None;
        effective_after = Some(chrono::Utc::now().timestamp_millis() - 24 * 60 * 60 * 1000);
        label = "last 24 hours".into();
    }

    let entries = repo.get_replay_entries(
        session_filter.as_deref(),
        effective_after,
        before_ms,
        tag_id,
        exit_code,
        executor,
        cwd_filter.as_deref(),
    )?;

    if entries.is_empty() {
        println!("\n  No commands found for: {label}");
        return Ok(());
    }

    let home = dirs_home();

    // Header
    let total = entries.len();
    println!("\n\x1b[1m── Replay: {label} ─────────────────────────────────\x1b[0m");

    // Session info if scoped to a session
    if let Some(ref sid) = session_filter {
        if let Ok(Some(sess)) = repo.get_session(sid) {
            let tag_info = if let Ok(Some(tname)) = repo.get_tag_by_session(sid) {
                format!("  │  Tag: {tname}")
            } else {
                String::new()
            };
            println!(
                "   Session {}  │  Host: {}{tag_info}  │  {} commands",
                &sess.id[..sess.id.len().min(8)],
                sess.hostname,
                total
            );
        }
    }
    println!();

    // Entries
    let mut success_count: usize = 0;
    let mut total_duration: i64 = 0;

    for entry in &entries {
        let time = chrono::DateTime::from_timestamp_millis(entry.started_at).map_or_else(
            || "??:??:??".into(),
            |dt| {
                dt.with_timezone(&chrono::Local)
                    .format("%H:%M:%S")
                    .to_string()
            },
        );

        let dir = shorten_path(&entry.cwd, &home);
        let duration = format_duration_ms(entry.duration_ms);

        let (status, status_color) = match entry.exit_code {
            Some(0) => {
                success_count += 1;
                ("\u{2713}".to_string(), "32") // green checkmark
            }
            Some(code) => (format!("\u{2717}{code}"), "31"), // red X + code
            None => ("\u{2022}".to_string(), "33"),          // yellow bullet
        };

        total_duration += entry.duration_ms;

        println!(
            " {time}  {dir:<20}  \x1b[{status_color}m{status:<4}\x1b[0m \x1b[2m{duration:>7}\x1b[0m  {cmd}",
            cmd = entry.command
        );
    }

    // Footer summary
    let failed = total - success_count;
    let avg_duration = if total > 0 {
        total_duration / total as i64
    } else {
        0
    };
    println!(
        "\n\x1b[1m── {} commands  │  {} passed  │  {} failed  │  Avg {} ──\x1b[0m\n",
        total,
        success_count,
        failed,
        format_duration_ms(avg_duration)
    );

    Ok(())
}

fn handle_stats_tui(days: Option<usize>, top: usize) -> Result<(), Box<dyn std::error::Error>> {
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
fn handle_stats_text(days: Option<usize>, top: usize) -> Result<(), Box<dyn std::error::Error>> {
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

#[allow(clippy::too_many_lines)]
fn handle_uninstall() -> Result<(), Box<dyn std::error::Error>> {
    // Check if installed via Homebrew
    let is_homebrew = std::process::Command::new("brew")
        .args(["list", "suvadu"])
        .output()
        .is_ok_and(|o| o.status.success());

    if is_homebrew {
        println!("Suvadu was installed via Homebrew.");
        println!("To uninstall, run:");
        println!();
        println!("  brew uninstall suvadu");
        println!();
        println!("Then remove shell hooks:");
        println!("  - Remove 'eval \"$(suv init zsh)\"' from ~/.zshrc");
        println!("  - Remove 'eval \"$(suv init bash)\"' from ~/.bashrc (if added)");
        println!();
        println!("If you used Claude Code integration:");
        println!("  - Delete ~/.config/suvadu/hooks/");
        println!("  - Remove the Suvadu hook entry from ~/.claude/settings.json");
        println!();
        println!("Your database and config files will NOT be removed.");
        println!("To remove them, delete:");
        println!("  - ~/Library/Application Support/suvadu/ (macOS)");
        return Ok(());
    }

    println!("WARNING: This will uninstall Suvadu from your system.");

    // Find all binary locations
    let mut paths_to_remove = vec![
        "/usr/local/bin/suv".to_string(),
        "/usr/local/bin/suvadu".to_string(),
    ];

    // Also detect the actual binary path
    if let Ok(output) = std::process::Command::new("which").arg("suv").output() {
        if output.status.success() {
            let real_path = String::from_utf8_lossy(&output.stdout).trim().to_string();
            if !paths_to_remove.contains(&real_path) {
                paths_to_remove.push(real_path);
            }
        }
    }

    println!("The following files will be removed:");
    for p in &paths_to_remove {
        println!("  - {p}");
    }
    println!();
    print!("Are you sure you want to continue? [y/N] ");
    std::io::Write::flush(&mut std::io::stdout())?;

    let mut input = String::new();
    std::io::stdin().read_line(&mut input)?;

    if input.trim().to_lowercase() != "y" {
        println!("Uninstall cancelled.");
        return Ok(());
    }

    println!("Requesting sudo access to remove files...");

    let mut all_ok = true;
    for path in &paths_to_remove {
        if std::path::Path::new(path).exists() {
            let status = std::process::Command::new("sudo")
                .args(["rm", "-f", path])
                .status()?;
            if !status.success() {
                eprintln!("Warning: Failed to remove {path}");
                all_ok = false;
            }
        }
    }

    if all_ok {
        println!("✓ Suvadu has been successfully uninstalled.");

        // Try to cleanup .zshrc
        if let Err(e) = util::cleanup_zshrc() {
            eprintln!("Warning: Failed to clean up .zshrc: {e}");
            println!("Please manually remove 'eval \"$(suv init zsh)\"' from your ~/.zshrc");
        } else {
            println!("✓ Removed shell integration from ~/.zshrc");
        }

        // Try to cleanup .bashrc
        if let Err(e) = util::cleanup_bashrc() {
            eprintln!("Warning: Failed to clean up .bashrc: {e}");
        } else {
            println!("✓ Removed shell integration from ~/.bashrc");
        }

        // Remove Claude Code hook scripts
        if let Ok(home) = std::env::var("HOME") {
            let hooks_dir = std::path::PathBuf::from(&home)
                .join(".config")
                .join("suvadu")
                .join("hooks");
            if hooks_dir.exists() {
                if let Err(e) = std::fs::remove_dir_all(&hooks_dir) {
                    eprintln!("Warning: Failed to remove hooks directory: {e}");
                } else {
                    println!("✓ Removed Claude Code hook scripts");
                }
            }
        }

        // Remove Suvadu entry from ~/.claude/settings.json
        match util::cleanup_claude_settings() {
            Ok(true) => println!("✓ Removed Suvadu hook from ~/.claude/settings.json"),
            Ok(false) => {}
            Err(e) => eprintln!("Warning: Failed to clean up Claude settings: {e}"),
        }

        println!("Note: Your database and config files were NOT removed.");
        println!("To remove them, delete:");
        println!("  - ~/.config/suvadu/ (Linux)");
        println!("  - ~/Library/Application Support/suvadu/ (macOS)");
    } else {
        eprintln!("Error: Failed to remove some files. Please try manually.");
        process::exit(1);
    }

    Ok(())
}

/// Normalize a timestamp to milliseconds.
/// Detects microsecond timestamps (16+ digits) and converts them.
/// Detects second timestamps (10 digits) and converts them.
/// Returns 0 unchanged (handled separately).
fn normalize_timestamp(ts: i64) -> i64 {
    if ts <= 0 {
        return ts;
    }
    // Microseconds: 16 digits (> 9_999_999_999_999 i.e. > ~Nov 2286 in ms)
    if ts > 9_999_999_999_999 {
        return ts / 1000;
    }
    // Seconds: 10 digits (< 100_000_000_000 i.e. < ~1973 in ms)
    // Current epoch seconds are ~1.7 billion (10 digits), ms would be 13 digits
    if ts < 100_000_000_000 {
        return ts * 1000;
    }
    // Already milliseconds
    ts
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn handle_add_with_context(
    session_id: &str,
    command: String,
    cwd: String,
    exit_code: Option<i32>,
    started_at: i64,
    ended_at: i64,
    executor_type: Option<String>,
    executor: Option<String>,
    context: Option<std::collections::HashMap<String, String>>,
) -> Result<(), Box<dyn std::error::Error>> {
    // Check config
    let config = config::load_config()?;
    if !config.enabled || config::is_paused() {
        return Ok(());
    }

    // Sanitize: ignore commands starting with space (privacy feature)
    if command.starts_with(' ') {
        return Ok(()); // Silently skip
    }

    // Check exclusions
    if util::is_excluded(&command, &config.exclusions) {
        return Ok(());
    }

    // Initialize database
    let db_path = db::get_db_path()?;
    let conn = db::init_db(&db_path)?;
    let repo = Repository::new(conn);

    // Auto-Tagging Logic (Path-based)
    let mut matched_tag_id: Option<i64> = None;
    if !config.auto_tags.is_empty() {
        if let Some(tag_name) = util::resolve_auto_tag(&cwd, &config.auto_tags) {
            // Check if tag exists, verify/create it
            let tags = repo.get_tags()?;
            if let Some(existing) = tags.iter().find(|t| t.name == tag_name.to_lowercase()) {
                matched_tag_id = Some(existing.id);
            } else {
                // Auto-create tag if configured in config but missing in DB
                // This is safe because user explicitly put it in auto_tags config
                if let Ok(id) = repo.create_tag(&tag_name, Some("Auto-created from path config")) {
                    matched_tag_id = Some(id);
                }
            }
        }
    }

    // Normalize timestamps to milliseconds (guards against micro/nanosecond inputs)
    let started_at = normalize_timestamp(started_at);
    let ended_at = normalize_timestamp(ended_at);

    // If started_at is still 0, use ended_at; if both 0, use current time
    let started_at = if started_at == 0 {
        if ended_at > 0 {
            ended_at
        } else {
            chrono::Utc::now().timestamp_millis()
        }
    } else {
        started_at
    };
    let ended_at = if ended_at == 0 { started_at } else { ended_at };

    // Create entry
    let mut entry = Entry::new(
        session_id.to_string(),
        command,
        cwd,
        exit_code,
        started_at,
        ended_at,
    )
    .with_tag_id(matched_tag_id);

    // Set executor information
    entry.executor_type = executor_type;
    entry.executor = executor;
    entry.context = context;

    // Ensure session exists
    if repo.get_session(session_id)?.is_none() {
        let session = Session {
            id: session_id.to_string(),
            hostname: hostname::get()?.to_string_lossy().to_string(),
            created_at: started_at,
            tag_id: None,
        };
        repo.insert_session(&session)?;
    }

    // Insert entry
    repo.insert_entry(&entry)?;

    Ok(()) // Silent success
}

#[allow(clippy::too_many_arguments)]
fn handle_add(
    session_id: &str,
    command: String,
    cwd: String,
    exit_code: Option<i32>,
    started_at: i64,
    ended_at: i64,
    executor_type: Option<String>,
    executor: Option<String>,
) -> Result<(), Box<dyn std::error::Error>> {
    handle_add_with_context(
        session_id,
        command,
        cwd,
        exit_code,
        started_at,
        ended_at,
        executor_type,
        executor,
        None,
    )
}

#[allow(clippy::too_many_arguments)]
fn handle_search(
    query: Option<&String>,
    unique: bool,
    after: Option<&str>,
    before: Option<&str>,
    tag: Option<&str>,
    exit_code: Option<i32>,
    executor: Option<&str>,
    here: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    // Check if recording is enabled/active
    // If not, we want to fallback to the shell's default search.
    // We use exit code 10 to signal this to the shell widget.
    if !config::should_record()? {
        process::exit(10);
    }

    // Initialize database
    let db_path = db::get_db_path()?;
    let conn = db::init_db(&db_path)?;
    let repo = Repository::new(conn);
    let app_config = config::load_config()?;

    // Auto-filter by session tag if enabled and no explicit tag provided
    let mut resolved_tag = tag.map(String::from);
    if tag.is_none() && app_config.search.filter_by_current_session_tag {
        if let Ok(session_id) = std::env::var("SUVADU_SESSION_ID") {
            if let Ok(Some(current_tag)) = repo.get_tag_by_session(&session_id) {
                resolved_tag = Some(current_tag);
            }
        }
    }

    // Resolve --here flag to current directory path
    let cwd_filter = if here {
        Some(std::env::current_dir()?.to_string_lossy().to_string())
    } else {
        None
    };

    // Run TUI
    let selected = search::run_search(
        &repo,
        query.map(String::as_str),
        unique,
        after,
        before,
        resolved_tag.as_deref(),
        exit_code,
        executor,
        false, // TUI uses substring matching
        cwd_filter.as_deref(),
    )?;

    // Output selected command to stdout (for shell to execute)
    if let Some(cmd) = selected {
        println!("{cmd}");
    }

    Ok(())
}

fn handle_get(
    query: &str,
    offset: usize,
    prefix: bool,
    cwd: Option<&str>,
) -> Result<(), Box<dyn std::error::Error>> {
    let db_path = db::get_db_path()?;
    let conn = db::init_db(&db_path)?;
    let repo = Repository::new(conn);

    let query_opt = if query.is_empty() { None } else { Some(query) };

    // When CWD is provided and context_boost is enabled, use recency-based
    // ranking that prefers same-directory commands. Otherwise fall back to
    // plain recency.
    let config = config::load_config().unwrap_or_default();
    let boost_cwd = if config.search.context_boost {
        cwd
    } else {
        None
    };

    let results = if boost_cwd.is_some() {
        repo.get_frecent_entries(1, offset, query_opt, prefix, boost_cwd)?
    } else {
        repo.get_unique_entries(
            1, offset, None, None, None, None, query_opt, prefix, false, None, None,
        )?
    };

    if let Some((entry, _)) = results.first() {
        print!("{}", entry.command);
    } // Else print nothing

    Ok(())
}

fn handle_delete(
    pattern: &str,
    is_regex: bool,
    dry_run: bool,
    before: Option<&str>,
) -> Result<(), Box<dyn std::error::Error>> {
    let db_path = db::get_db_path()?;
    let conn = db::init_db(&db_path)?;
    let repo = Repository::new(conn);

    let before_timestamp: Option<i64> = if let Some(date_str) = before {
        Some(util::parse_date_input(date_str, false).ok_or_else(|| {
            format!("Invalid date format: {date_str}. Use YYYY-MM-DD or keywords.")
        })?)
    } else {
        None
    };

    if dry_run {
        let count = repo.count_entries_by_pattern(pattern, is_regex, before_timestamp)?;
        if count == 0 {
            println!("No entries matched the pattern '{pattern}'");
        } else {
            println!("Dry Run: {count} entries match the pattern '{pattern}'.");
            if let Some(ts) = before_timestamp {
                let date = chrono::DateTime::from_timestamp_millis(ts)
                    .ok_or_else(|| format!("Invalid timestamp: {ts}"))?;
                println!(
                    "(Filtered entries older than: {})",
                    date.format("%Y-%m-%d %H:%M:%S")
                );
            }
        }
    } else {
        println!("Deleting entries matching pattern '{pattern}'...");
        if let Some(ts) = before_timestamp {
            let date = chrono::DateTime::from_timestamp_millis(ts)
                .ok_or_else(|| format!("Invalid timestamp: {ts}"))?;
            println!("  and older than: {}", date.format("%Y-%m-%d %H:%M:%S"));
        }
        let deleted = repo.delete_entries(pattern, is_regex, before_timestamp)?;
        println!("✓ Deleted {deleted} entries.");
    }

    Ok(())
}

fn handle_settings() -> Result<(), Box<dyn std::error::Error>> {
    let config = config::load_config()?;

    // Setup TUI
    crossterm::terminal::enable_raw_mode()?;
    let mut stdout = std::io::stdout();
    crossterm::execute!(stdout, crossterm::terminal::EnterAlternateScreen)?;
    let backend = ratatui::backend::CrosstermBackend::new(stdout);
    let mut terminal = ratatui::Terminal::new(backend)?;

    // Run Settings Loop
    let res = settings_ui::run_settings_ui(&mut terminal, config);

    // Restore terminal
    crossterm::terminal::disable_raw_mode()?;
    crossterm::execute!(
        terminal.backend_mut(),
        crossterm::terminal::LeaveAlternateScreen
    )?;
    terminal.show_cursor()?;

    if let Err(e) = res {
        eprintln!("Error in settings UI: {e}");
    }

    Ok(())
}

fn handle_status() -> Result<(), Box<dyn std::error::Error>> {
    let global_enabled = config::is_enabled()?;
    let is_paused = config::is_paused();
    let recording = global_enabled && !is_paused;

    println!("Suvadu Status:");
    println!(
        "  Global Config:   {}",
        if global_enabled {
            "✅ Enabled"
        } else {
            "❌ Disabled"
        }
    );
    println!(
        "  Current Session: {}",
        if is_paused {
            "⏸️  Paused"
        } else {
            "▶️  Active"
        }
    );

    println!();
    if recording {
        println!("History IS being recorded.");
    } else {
        println!("History is NOT being recorded.");
    }

    // Session Info
    if let Ok(session_id) = std::env::var("SUVADU_SESSION_ID") {
        println!("\nSession Details:");
        println!("  ID: {session_id}");

        // Try to fetch tag info
        // We warn but don't fail if DB isn't accessible just for status
        if let Ok(db_path) = db::get_db_path() {
            if let Ok(conn) = db::init_db(&db_path) {
                let repo = Repository::new(conn);
                if let Ok(Some(session)) = repo.get_session(&session_id) {
                    let tag_display = if let Some(tag_id) = session.tag_id {
                        // We need to fetch tag name.
                        // Repository doesn't have a direct get_tag(id) but get_tags() is cheap enough or we can add one.
                        // But get_unique_entries joins it.
                        // Let's just use get_tags loop for now or simple query.
                        repo.get_tags()
                            .ok()
                            .and_then(|tags| {
                                tags.into_iter().find(|t| t.id == tag_id).map(|t| t.name)
                            })
                            .unwrap_or_else(|| format!("ID: {tag_id} (Unknown)"))
                    } else {
                        "None".to_string()
                    };
                    println!("  Tag: {tag_display}");
                }
            }
        }
    } else {
        println!("\nSession Details: No SUVADU_SESSION_ID found in environment.");
    }

    Ok(())
}

fn handle_tag(cmd: cli::TagCommands) -> Result<(), Box<dyn std::error::Error>> {
    let db_path = db::get_db_path()?;
    let conn = db::init_db(&db_path)?;
    let repo = Repository::new(conn);

    match cmd {
        cli::TagCommands::Create { name, description } => {
            handle_tag_create(&repo, name, description.as_deref())?;
        }
        cli::TagCommands::List => {
            handle_tag_list(&repo)?;
        }
        cli::TagCommands::Associate {
            tag_name,
            session_id,
        } => {
            handle_tag_associate(&repo, &tag_name, session_id)?;
        }
        cli::TagCommands::Update {
            name,
            new_name,
            description,
        } => {
            handle_tag_update(&repo, &name, new_name.as_deref(), description.as_deref())?;
        }
    }
    Ok(())
}

fn handle_bookmark(cmd: cli::BookmarkCommands) -> Result<(), Box<dyn std::error::Error>> {
    let db_path = db::get_db_path()?;
    let conn = db::init_db(&db_path)?;
    let repo = Repository::new(conn);

    match cmd {
        cli::BookmarkCommands::Add { command, label } => {
            repo.add_bookmark(&command, label.as_deref())?;
            println!("Bookmarked: {command}");
        }
        cli::BookmarkCommands::List => {
            let bookmarks = repo.list_bookmarks()?;
            if bookmarks.is_empty() {
                println!("No bookmarks yet. Use `suv bookmark add <command>` to save one.");
            } else {
                println!("\x1b[1m{:<50} {:<20} Added\x1b[0m", "Command", "Label");
                for bm in &bookmarks {
                    let date = chrono::DateTime::from_timestamp_millis(bm.created_at)
                        .map(|dt| dt.format("%Y-%m-%d").to_string())
                        .unwrap_or_default();
                    let label_str = bm.label.as_deref().unwrap_or("-");
                    let cmd_display = if bm.command.len() > 48 {
                        format!("{}…", &bm.command[..47])
                    } else {
                        bm.command.clone()
                    };
                    println!("{cmd_display:<50} {label_str:<20} {date}");
                }
                println!("\n{} bookmark(s)", bookmarks.len());
            }
        }
        cli::BookmarkCommands::Remove { command } => {
            if repo.remove_bookmark(&command)? {
                println!("Removed bookmark: {command}");
            } else {
                eprintln!("No bookmark found for: {command}");
            }
        }
    }
    Ok(())
}

fn handle_note(
    entry_id: i64,
    content: Option<String>,
    delete: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    let db_path = db::get_db_path()?;
    let conn = db::init_db(&db_path)?;
    let repo = Repository::new(conn);

    if delete {
        if repo.delete_note(entry_id)? {
            println!("Note deleted for entry {entry_id}.");
        } else {
            eprintln!("No note found for entry {entry_id}.");
        }
    } else if let Some(text) = content {
        repo.upsert_note(entry_id, &text)?;
        println!("Note saved for entry {entry_id}.");
    } else {
        match repo.get_note(entry_id)? {
            Some(note) => println!("{}", note.content),
            None => eprintln!("No note for entry {entry_id}."),
        }
    }
    Ok(())
}

fn handle_tag_create(
    repo: &Repository,
    name: Option<String>,
    description: Option<&str>,
) -> Result<(), Box<dyn std::error::Error>> {
    let name = if let Some(n) = name {
        n
    } else {
        // Interactive prompt if name missing
        print!("Enter tag name: ");
        std::io::Write::flush(&mut std::io::stdout())?;
        let mut buffer = String::new();
        std::io::stdin().read_line(&mut buffer)?;
        buffer.trim().to_string()
    };

    if name.is_empty() {
        eprintln!("Tag name cannot be empty.");
        return Ok(());
    }

    match repo.create_tag(&name, description) {
        Ok(_) => println!("✓ Tag '{}' created", name.to_lowercase()),
        Err(e) => {
            if let db::DbError::Sqlite(rusqlite::Error::SqliteFailure(err, _)) = &e {
                if err.code == rusqlite::ErrorCode::ConstraintViolation {
                    eprintln!("Error: Tag '{name}' already exists.");
                    return Ok(());
                }
            }
            eprintln!("Error creating tag: {e}");
        }
    }
    Ok(())
}

fn handle_tag_list(repo: &Repository) -> Result<(), Box<dyn std::error::Error>> {
    let tags = repo.get_tags()?;
    if tags.is_empty() {
        println!("No tags found.");
    } else {
        // Calculate widths
        let max_name = tags.iter().map(|t| t.name.len()).max().unwrap_or(4).max(4);
        let max_desc = tags
            .iter()
            .map(|t| t.description.as_deref().unwrap_or("").len())
            .max()
            .unwrap_or(11)
            .max(11);

        let w_name = max_name + 2;
        let w_desc = max_desc + 2;

        let sep = format!("+{}+{}+", "-".repeat(w_name), "-".repeat(w_desc));

        println!("{sep}");
        println!("| {:<w_name$} | {:<w_desc$} |", "NAME", "DESCRIPTION");
        println!("{sep}");

        for tag in tags {
            println!(
                "| {:<w_name$} | {:<w_desc$} |",
                tag.name,
                tag.description.as_deref().unwrap_or("")
            );
        }
        println!("{sep}");
    }
    Ok(())
}

fn handle_tag_associate(
    repo: &Repository,
    tag_name: &str,
    session_id: Option<String>,
) -> Result<(), Box<dyn std::error::Error>> {
    // Find tag or create it
    let tags = repo.get_tags()?;
    let existing_tag = tags.iter().find(|t| t.name == tag_name.to_lowercase());

    let tag_id = if let Some(t) = existing_tag {
        t.id
    } else {
        // Try to create it
        println!("Tag '{tag_name}' not found. Creating it...");
        match repo.create_tag(tag_name, None) {
            Ok(id) => {
                println!("✓ Tag '{tag_name}' created");
                id
            }
            Err(e) => {
                if let db::DbError::Validation(ref msg) = e {
                    eprintln!("Error: {msg}");
                } else {
                    eprintln!("Error creating tag: {e}");
                }
                return Ok(());
            }
        }
    };

    let sid = session_id
        .or_else(|| std::env::var("SUVADU_SESSION_ID").ok())
        .unwrap_or_default();

    if sid.is_empty() {
        eprintln!("No session ID provided or found in env.");
        return Ok(());
    }

    repo.tag_session(&sid, Some(tag_id))?;
    println!("✓ Session '{sid}' associated with tag '{tag_name}'");
    Ok(())
}

fn handle_tag_update(
    repo: &Repository,
    name: &str,
    new_name: Option<&str>,
    description: Option<&str>,
) -> Result<(), Box<dyn std::error::Error>> {
    let tags = repo.get_tags()?;
    let tag = tags.iter().find(|t| t.name == name.to_lowercase());

    if let Some(t) = tag {
        let updated_name = new_name.unwrap_or(&t.name);
        let updated_desc = description.or(t.description.as_deref());

        match repo.update_tag(t.id, updated_name, updated_desc) {
            Ok(()) => println!("✓ Tag '{}' updated", t.name),
            Err(e) => eprintln!("Error updating tag: {e}"),
        }
    } else {
        eprintln!("Tag '{name}' not found.");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_normalize_timestamp_milliseconds() {
        // Already milliseconds (13 digits) — no change
        let ts = 1_770_693_885_695;
        assert_eq!(normalize_timestamp(ts), ts);
    }

    #[test]
    fn test_normalize_timestamp_microseconds() {
        // Microseconds (16 digits) → divide by 1000
        let ts_us = 1_770_574_211_585_923;
        let ts_ms = ts_us / 1000;
        assert_eq!(normalize_timestamp(ts_us), ts_ms);
    }

    #[test]
    fn test_normalize_timestamp_seconds() {
        // Seconds (10 digits) → multiply by 1000
        let ts_s = 1_770_693_885;
        assert_eq!(normalize_timestamp(ts_s), ts_s * 1000);
    }

    #[test]
    fn test_normalize_timestamp_zero() {
        assert_eq!(normalize_timestamp(0), 0);
    }
}
