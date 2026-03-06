use std::process;

use crate::config;
use crate::db;
use crate::repository::Repository;
use crate::settings_ui;
use crate::util;

pub fn handle_settings() -> Result<(), Box<dyn std::error::Error>> {
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

pub fn handle_status() -> Result<(), Box<dyn std::error::Error>> {
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
                    let tag_display = session.tag_id.map_or_else(
                        || "None".to_string(),
                        |tag_id| {
                            repo.get_tags()
                                .ok()
                                .and_then(|tags| {
                                    tags.into_iter().find(|t| t.id == tag_id).map(|t| t.name)
                                })
                                .unwrap_or_else(|| format!("ID: {tag_id} (Unknown)"))
                        },
                    );
                    println!("  Tag: {tag_display}");
                }
            }
        }
    } else {
        println!("\nSession Details: No SUVADU_SESSION_ID found in environment.");
    }

    Ok(())
}

#[allow(clippy::too_many_lines)]
pub fn handle_uninstall() -> Result<(), Box<dyn std::error::Error>> {
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
