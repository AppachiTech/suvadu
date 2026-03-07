use clap::Parser;
use std::process;

mod agent;
mod agent_ui;
mod cli;
mod commands;
mod config;
mod db;
mod hooks;
mod import_export;
mod integrations;
mod models;
mod redact;
mod repository;
mod risk;
mod search;
mod session_ui;
mod settings_ui;
mod stats_ui;
mod suggest;
mod suggest_ui;
#[cfg(test)]
mod test_utils;
mod theme;
mod update;
mod util;

use cli::{Cli, Commands};

fn main() {
    // Install a panic handler that restores the terminal from raw mode.
    // Without this, a panic in a TUI screen leaves the terminal unusable.
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let _ = crossterm::terminal::disable_raw_mode();
        let _ = crossterm::execute!(
            std::io::stderr(),
            crossterm::event::DisableMouseCapture,
            crossterm::terminal::LeaveAlternateScreen
        );
        default_hook(info);
    }));

    let cli = Cli::parse();

    if let Err(e) = run(cli) {
        eprintln!("Error: {e}");
        process::exit(1);
    }
}

fn run(cli: Cli) -> Result<(), Box<dyn std::error::Error>> {
    print_setup_hint(&cli.command);

    // Initialize theme only for user-facing commands.
    // Internal commands (Add, Get, hooks, etc.) don't render TUI,
    // so skip the config read + theme init on the hot path.
    if is_user_facing_command(&cli.command) {
        let theme_name = config::load_config().map(|c| c.theme).unwrap_or_default();
        theme::init_theme(theme_name);
    }

    run_command(cli.command)
}

#[allow(clippy::too_many_lines)]
fn run_command(command: Commands) -> Result<(), Box<dyn std::error::Error>> {
    match command {
        Commands::Enable => run_toggle(true),
        Commands::Disable => run_toggle(false),
        Commands::Pause => {
            println!("export SUVADU_PAUSED=1");
            Ok(())
        }
        cmd @ Commands::Add { .. } => run_add(cmd),
        Commands::Init { target } => run_init(&target),
        Commands::HookClaudeCode => integrations::handle_hook_claude_code(),
        Commands::HookClaudePrompt => integrations::handle_hook_claude_prompt(),
        cmd @ Commands::Search { .. } => run_search(cmd),
        Commands::Get {
            query,
            offset,
            prefix,
            cwd,
        } => commands::search::handle_get(&query, offset, prefix, cwd.as_deref()),
        Commands::Settings => commands::settings::handle_settings(),
        Commands::Status => commands::settings::handle_status(),
        Commands::Tag(cmd) => commands::tag::handle_tag(cmd),
        Commands::Bookmark(cmd) => commands::entry::handle_bookmark(cmd),
        Commands::Alias(cmd) => commands::alias::handle_alias(cmd),
        Commands::Note {
            entry_id,
            content,
            delete,
        } => commands::entry::handle_note(entry_id, content, delete),
        Commands::Delete {
            pattern,
            regex,
            dry_run,
            yes,
            before,
        } => commands::entry::handle_delete(&pattern, regex, dry_run, yes, before.as_deref()),
        Commands::Gc { dry_run, vacuum } => commands::entry::handle_gc(dry_run, vacuum),
        Commands::Uninstall => commands::settings::handle_uninstall(),
        Commands::Version => {
            let hash = env!("BUILD_HASH");
            if hash.is_empty() {
                println!(
                    "suvadu v{} ({})",
                    env!("CARGO_PKG_VERSION"),
                    env!("BUILD_DATE")
                );
            } else {
                println!(
                    "suvadu v{} ({} {})",
                    env!("CARGO_PKG_VERSION"),
                    env!("BUILD_DATE"),
                    hash
                );
            }
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
        } => commands::wrap::handle_wrap(&command, &executor_type, &executor),
        Commands::Export {
            format,
            after,
            before,
        } => import_export::handle_export(format.as_str(), after.as_deref(), before.as_deref()),
        Commands::Import {
            file,
            from,
            dry_run,
        } => run_import(&file, from, dry_run),
        Commands::Stats {
            days,
            top,
            text,
            json,
            tag,
        } => {
            if json {
                commands::stats::handle_stats_json(days, top, tag.as_deref())
            } else if text {
                commands::stats::handle_stats_text(days, top, tag.as_deref())
            } else {
                commands::stats::handle_stats_tui(days, top, tag.as_deref())
            }
        }
        cmd @ Commands::Replay { .. } => run_replay(cmd),
        Commands::Session {
            session_id,
            list,
            after,
            tag,
            limit,
        } => commands::session::handle_session(
            session_id.as_deref(),
            list,
            after.as_deref(),
            tag.as_deref(),
            limit,
        ),
        Commands::Agent(cmd) => agent::handle_agent(cmd),
    }
}

fn run_add(cmd: Commands) -> Result<(), Box<dyn std::error::Error>> {
    let Commands::Add {
        session_id,
        command,
        cwd,
        exit_code,
        started_at,
        ended_at,
        executor_type,
        executor,
    } = cmd
    else {
        unreachable!()
    };
    commands::entry::handle_add_with_context(commands::entry::AddParams {
        session_id,
        command,
        cwd,
        exit_code,
        started_at,
        ended_at,
        executor_type,
        executor,
        context: None,
    })
}

fn run_search(cmd: Commands) -> Result<(), Box<dyn std::error::Error>> {
    let Commands::Search {
        query,
        unique,
        after,
        before,
        tag,
        exit_code,
        executor,
        here,
        field,
    } = cmd
    else {
        unreachable!()
    };
    commands::search::handle_search(&commands::search::SearchParams {
        query: query.as_ref(),
        unique,
        after: after.as_deref(),
        before: before.as_deref(),
        tag: tag.as_deref(),
        exit_code,
        executor: executor.as_deref(),
        here,
        field: field.as_str(),
    })
}

fn run_replay(cmd: Commands) -> Result<(), Box<dyn std::error::Error>> {
    let Commands::Replay {
        session,
        after,
        before,
        tag,
        exit_code,
        executor,
        here,
        cwd,
    } = cmd
    else {
        unreachable!()
    };
    commands::replay::handle_replay(&commands::replay::ReplayParams {
        session: session.as_deref(),
        after: after.as_deref(),
        before: before.as_deref(),
        tag: tag.as_deref(),
        exit_code,
        executor: executor.as_deref(),
        here,
        cwd: cwd.as_deref(),
    })
}

fn run_toggle(enable: bool) -> Result<(), Box<dyn std::error::Error>> {
    let mut cfg = config::load_config()?;
    cfg.enabled = enable;
    config::save_config(&cfg)?;
    let word = if enable { "enabled" } else { "disabled" };
    println!("✓ History recording {word} globally");
    Ok(())
}

fn run_init(target: &str) -> Result<(), Box<dyn std::error::Error>> {
    match target {
        "zsh" => {
            let cfg = config::load_config().unwrap_or_default();
            println!("{}", hooks::get_zsh_hook(&cfg)?);
            print_first_run_tip();
            Ok(())
        }
        "bash" => {
            let cfg = config::load_config().unwrap_or_default();
            println!("{}", hooks::get_bash_hook(&cfg)?);
            print_first_run_tip();
            Ok(())
        }
        "claude-code" => integrations::handle_init_claude_code(),
        "cursor" => integrations::handle_init_ide(
            "Cursor",
            "Suvadu detects Cursor via $CURSOR_INJECTION and\n$CURSOR_TRACE_ID environment variables.",
            "cursor",
        ),
        "antigravity" => integrations::handle_init_ide(
            "Antigravity",
            "Suvadu detects Antigravity via the $ANTIGRAVITY_AGENT\nenvironment variable.",
            "antigravity",
        ),
        _ => {
            eprintln!(
                "Unsupported target: {target}. Use 'zsh', 'bash', 'claude-code', 'cursor', or 'antigravity'."
            );
            process::exit(1);
        }
    }
}

fn run_import(
    file: &str,
    from: cli::ImportFormat,
    dry_run: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    match from {
        cli::ImportFormat::Jsonl => import_export::handle_import(file, dry_run),
        cli::ImportFormat::ZshHistory => import_export::handle_import_zsh_history(file, dry_run),
    }
}

/// Check if a command should show the setup hint (skip internal/setup commands).
const fn is_user_facing_command(cmd: &Commands) -> bool {
    !matches!(
        cmd,
        Commands::Init { .. }
            | Commands::Add { .. }
            | Commands::Get { .. }
            | Commands::HookClaudeCode
            | Commands::HookClaudePrompt
            | Commands::Completions { .. }
            | Commands::Man
            | Commands::Wrap { .. }
    )
}

/// Show setup instructions when the database doesn't exist yet (fresh install).
/// Prints to stderr so it doesn't interfere with stdout.
fn print_setup_hint(cmd: &Commands) {
    if !is_user_facing_command(cmd) {
        return;
    }
    if let Ok(db_path) = db::get_db_path() {
        if !db_path.exists() {
            eprintln!();
            eprintln!("  Welcome to Suvadu!");
            eprintln!();
            eprintln!("  Quick setup — add to your shell config:");
            eprintln!("    echo 'eval \"$(suv init zsh)\"' >> ~/.zshrc && source ~/.zshrc");
            eprintln!();
            eprintln!("  Or for Bash:");
            eprintln!("    echo 'eval \"$(suv init bash)\"' >> ~/.bashrc && source ~/.bashrc");
            eprintln!();
            eprintln!("  Track AI agent commands:");
            eprintln!("    suv init claude-code    Claude Code");
            eprintln!("    suv init cursor         Cursor");
            eprintln!("    suv init antigravity    Antigravity");
            eprintln!();
        }
    }
}

/// Show agent integration tips during `suv init zsh/bash` (first run only).
/// Prints to stderr so it doesn't interfere with shell hook eval on stdout.
fn print_first_run_tip() {
    if let Ok(db_path) = db::get_db_path() {
        if !db_path.exists() {
            eprintln!();
            eprintln!("  Suvadu installed successfully");
            eprintln!();
            eprintln!("  Track AI agent commands:");
            eprintln!("    suv init claude-code    Claude Code");
            eprintln!("    suv init cursor         Cursor");
            eprintln!("    suv init antigravity    Antigravity");
            eprintln!();
            eprintln!("  Try it: suv search");
        }
    }
}
