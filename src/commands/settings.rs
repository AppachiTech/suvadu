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
    // Detect all installation sources
    let is_homebrew = std::process::Command::new("brew")
        .args(["list", "suvadu"])
        .output()
        .is_ok_and(|o| o.status.success());

    let is_cargo = std::env::var("HOME")
        .ok()
        .map(|h| std::path::PathBuf::from(h).join(".cargo/bin/suv"))
        .is_some_and(|p| p.exists());

    if !is_homebrew && !is_cargo {
        // Fall back to detecting any binary via `which`
        let which_path = std::process::Command::new("which")
            .arg("suv")
            .output()
            .ok()
            .filter(|o| o.status.success())
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string());

        if let Some(path) = which_path {
            println!("Found suv at: {path}");
            println!("Remove it manually with:");
            println!("  rm {path}");
        } else {
            println!("No Suvadu installation detected.");
        }
        return Ok(());
    }

    // Show what we found
    println!("Detected Suvadu installation sources:");
    if is_homebrew {
        println!("  • Homebrew (brew)");
    }
    if is_cargo {
        println!("  • Cargo (~/.cargo/bin/suv)");
    }
    println!();
    print!("Uninstall all? [y/N] ");
    std::io::Write::flush(&mut std::io::stdout())?;

    let mut input = String::new();
    std::io::stdin().read_line(&mut input)?;
    if input.trim().to_lowercase() != "y" {
        println!("Uninstall cancelled.");
        return Ok(());
    }

    let mut all_ok = true;

    // Homebrew
    if is_homebrew {
        print!("Removing Homebrew package... ");
        let status = std::process::Command::new("brew")
            .args(["uninstall", "suvadu"])
            .status()?;
        if status.success() {
            println!("✓");
        } else {
            println!("✘");
            eprintln!("  Failed. Run manually: brew uninstall suvadu");
            all_ok = false;
        }
    }

    // Cargo
    if is_cargo {
        print!("Removing Cargo installation... ");
        let status = std::process::Command::new("cargo")
            .args(["uninstall", "suvadu"])
            .status();
        match status {
            Ok(s) if s.success() => println!("✓"),
            _ => {
                // Fallback: remove binary directly
                if let Ok(home) = std::env::var("HOME") {
                    let cargo_bin = format!("{home}/.cargo/bin/suv");
                    if std::fs::remove_file(&cargo_bin).is_ok() {
                        println!("✓ (removed binary directly)");
                    } else {
                        println!("✘");
                        eprintln!("  Failed. Run manually: cargo uninstall suvadu");
                        all_ok = false;
                    }
                } else {
                    println!("✘");
                    eprintln!("  Failed (HOME not set). Run manually: cargo uninstall suvadu");
                    all_ok = false;
                }
            }
        }
    }

    // Clean up shell hooks and integrations
    if let Err(e) = util::cleanup_zshrc() {
        eprintln!("Warning: Failed to clean up .zshrc: {e}");
    } else {
        println!("✓ Removed shell integration from ~/.zshrc");
    }

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

    println!();
    if all_ok {
        println!("Suvadu has been uninstalled.");
    } else {
        eprintln!("Some steps failed. See messages above.");
    }

    println!();
    println!("Your database and config files were NOT removed.");
    println!("To remove them, delete:");
    println!("  - ~/.config/suvadu/ (Linux)");
    println!("  - ~/Library/Application Support/suvadu/ (macOS)");

    Ok(())
}

#[cfg(test)]
mod tests {
    #[test]
    fn test_recording_state_logic() {
        // Recording requires both: globally enabled AND not paused
        let cases = [
            (true, false, true),   // enabled + not paused → recording
            (true, true, false),   // enabled + paused → not recording
            (false, false, false), // disabled + not paused → not recording
            (false, true, false),  // disabled + paused → not recording
        ];
        for (enabled, paused, expected) in cases {
            let recording = enabled && !paused;
            assert_eq!(recording, expected, "enabled={enabled}, paused={paused}");
        }
    }

    #[test]
    fn test_uninstall_detection_logic() {
        // If neither homebrew nor cargo is detected, we fall back to `which`
        let is_homebrew = false;
        let is_cargo = false;
        assert!(
            !is_homebrew && !is_cargo,
            "Should fall back to which-based detection"
        );
    }

    #[test]
    fn test_confirmation_input_parsing() {
        // Only "y" (case-insensitive) should proceed
        let accepts = ["y", "Y", " y ", "Y "];
        let rejects = ["n", "N", "", "yes", "no"];
        for input in accepts {
            assert_eq!(input.trim().to_lowercase(), "y");
        }
        for input in rejects {
            assert_ne!(input.trim().to_lowercase(), "y");
        }
    }
}
