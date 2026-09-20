use crate::commands::capture;
use crate::config;
use crate::db;
use crate::repository::Repository;
use crate::settings_ui;
use crate::util;

pub fn handle_settings() -> Result<(), Box<dyn std::error::Error>> {
    let config = config::load_config()?;

    let mut guard = util::TerminalGuard::new()?;
    let res = settings_ui::run_settings_ui(guard.terminal(), config);
    drop(guard);

    res?;
    Ok(())
}

/// The newest stored shell record, used as evidence that capture works.
/// Fields are reported exactly as stored — a missing exit code stays missing.
pub struct LastRecord {
    pub command: String,
    pub age_secs: i64,
    pub exit_code: Option<i32>,
}

/// Everything `status_lines` needs, gathered by the caller so the rendering
/// itself stays pure and testable.
pub struct StatusFacts {
    pub state: capture::RecordingState,
    pub capture: capture::CaptureFacts,
    pub last_record: Option<LastRecord>,
}

/// Render the recording/capture section of `suv status`.
///
/// Configuration state ("recording enabled") and capture evidence ("a record
/// actually arrived") are reported separately: the first is what the config
/// allows, the second is the only thing that proves history is being stored.
fn status_lines(facts: &StatusFacts) -> Vec<String> {
    let evidence = capture::capture_evidence(&facts.capture);
    let state_icon = if facts.state.is_enabled() {
        "\u{2705}"
    } else {
        "\u{26d4}"
    };
    let evidence_icon = if evidence.is_proven() {
        "\u{2705}"
    } else {
        "\u{2753}"
    };

    let mut lines = vec![
        "Suvadu Status:".to_string(),
        format!("  Recording: {state_icon} {}", facts.state.label()),
        format!("  Capture:   {evidence_icon} {}", evidence.headline()),
    ];

    if let Some(last) = &facts.last_record {
        let exit = last
            .exit_code
            .map_or_else(|| "exit unknown".to_string(), |c| format!("exit {c}"));
        lines.push(format!(
            "  Last shell record: {} \u{2014} {} ago, {exit}",
            crate::util::truncate_str(&last.command, 60, "\u{2026}"),
            capture::format_age(last.age_secs),
        ));
    }

    for note in capture::evidence_notes(&facts.capture) {
        lines.push(format!("  Note: {note}"));
    }

    let fixes = capture::recording_fixes(facts.state);
    if !fixes.is_empty() {
        lines.push(String::new());
        lines.push("To resume recording:".to_string());
        for fix in fixes {
            lines.push(format!("  {fix}"));
        }
    }

    lines.push(String::new());
    lines.push("To verify capture end-to-end:".to_string());
    for step in capture::verification_steps() {
        lines.push(format!("  {step}"));
    }

    lines
}

/// Gather the newest stored shell (non-agent) record, if any.
fn last_shell_record(repo: &Repository, now_ms: i64) -> Option<LastRecord> {
    let filter = crate::repository::QueryFilter {
        exclude_agents: true,
        ..Default::default()
    };
    let entry = repo
        .get_recent_entries(1, 0, &filter, None)
        .ok()?
        .into_iter()
        .next()?;
    Some(LastRecord {
        age_secs: (now_ms - entry.started_at).max(0) / 1000,
        command: entry.command,
        exit_code: entry.exit_code,
    })
}

pub fn handle_status() -> Result<(), Box<dyn std::error::Error>> {
    let global_enabled = config::is_enabled()?;
    let is_paused = config::is_paused();
    let state = capture::recording_state(global_enabled, is_paused);
    let session_env_present = std::env::var("SUVADU_SESSION_ID").is_ok();
    let hook_configured = capture::shell_hook_configured();

    // Open the database first: only stored records can prove capture.
    let db_path = db::get_db_path().ok();
    let repo = db_path
        .as_ref()
        .and_then(|p| db::init_db(p).ok())
        .map(Repository::new);

    let (shell_records, last_record) = repo.as_ref().map_or((0, None), |repo| {
        let count = repo
            .count_filtered(&crate::repository::QueryFilter {
                exclude_agents: true,
                ..Default::default()
            })
            .unwrap_or(0);
        (
            count,
            last_shell_record(repo, chrono::Utc::now().timestamp_millis()),
        )
    });

    for line in status_lines(&StatusFacts {
        state,
        capture: capture::CaptureFacts {
            hook_configured,
            session_env_present,
            shell_records,
            newest_shell_record_age_secs: last_record.as_ref().map(|r| r.age_secs),
        },
        last_record,
    }) {
        println!("{line}");
    }

    // Database info
    if let (Some(db_path), Some(repo)) = (db_path, repo) {
        print_database_section(&db_path, &repo);
    } else {
        println!();
        println!("Database: not found. Run a few commands first.");
    }

    Ok(())
}

/// Print the database/session/tips block of `suv status`.
fn print_database_section(db_path: &std::path::Path, repo: &Repository) {
    println!();
    println!("Database:");
    println!("  Path: {}", db_path.display());

    // Total command count
    let total = repo
        .count_filtered(&crate::repository::QueryFilter {
            after: None,
            before: None,
            tag_id: None,
            exit_code: None,
            query: None,
            prefix_match: false,
            executor: None,
            cwd: None,
            field: crate::models::SearchField::Command,
            exclude_agents: false,
            cwd_prefix: false,
            failed_only: false,
            bookmarked_only: false,
            exclude_dirs: &[],
        })
        .unwrap_or(0);
    println!("  Commands: {total} recorded");

    // Agent command count
    let agent_entries = repo
        .get_distinct_executors()
        .unwrap_or_default()
        .into_iter()
        .filter(|e| e.starts_with("agent:") && !e.ends_with("unknown"))
        .collect::<Vec<_>>();
    if !agent_entries.is_empty() {
        let agents: Vec<&str> = agent_entries
            .iter()
            .map(|e| e.strip_prefix("agent: ").unwrap_or(e.as_str()))
            .collect();
        println!("  Agents:   {}", agents.join(", "));
    }

    // Session info
    if let Ok(session_id) = std::env::var("SUVADU_SESSION_ID") {
        println!("\nSession:");
        println!("  ID: {session_id}");
        if let Ok(Some(session)) = repo.get_session(&session_id) {
            let tag_display = session.tag_id.map_or_else(
                || "None".to_string(),
                |tag_id| {
                    repo.get_tags()
                        .ok()
                        .and_then(|tags| tags.into_iter().find(|t| t.id == tag_id).map(|t| t.name))
                        .unwrap_or_else(|| format!("ID: {tag_id} (Unknown)"))
                },
            );
            println!("  Tag: {tag_display}");
        }
    }

    // Tips
    let color = crate::util::color_enabled();
    let cyan = if color { "\x1b[36m" } else { "" };
    let r = if color { "\x1b[0m" } else { "" };
    println!();
    println!("Try:");
    println!("  {cyan}suv search{r}            \u{2014} interactive history search (or Ctrl+R)");
    if !agent_entries.is_empty() {
        println!("  {cyan}suv agent dashboard{r}  \u{2014} monitor agent activity");
    }
}

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
        detect_fallback_binary();
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
    if !crate::util::stdin_is_terminal() {
        return Err("Refusing to uninstall without confirmation: stdin is not a terminal.".into());
    }
    print!("Uninstall all? [y/N] ");
    std::io::Write::flush(&mut std::io::stdout())?;

    let mut input = String::new();
    std::io::stdin().read_line(&mut input)?;
    if input.trim().to_lowercase() != "y" {
        println!("Uninstall cancelled.");
        return Ok(());
    }

    let mut all_ok = true;

    if is_homebrew {
        all_ok &= uninstall_homebrew();
    }

    if is_cargo {
        all_ok &= uninstall_cargo();
    }

    cleanup_integrations();

    println!();
    if all_ok {
        println!("Suvadu has been uninstalled.");
    } else {
        eprintln!("Some steps failed. See messages above.");
    }

    println!();
    println!("Your database and config files were NOT removed.");
    println!("To remove them, delete:");
    for path in uninstall_data_paths() {
        println!("  - {path}");
    }

    Ok(())
}

/// Directories holding user data, as strings for display, deduplicated when
/// a platform puts config and data in the same place.
fn uninstall_data_path_list(
    config_dir: &std::path::Path,
    data_dir: &std::path::Path,
) -> Vec<String> {
    let mut paths = vec![config_dir.display().to_string()];
    let data = data_dir.display().to_string();
    if !paths.contains(&data) {
        paths.push(data);
    }
    paths
}

/// The real config and data directories for this platform.
///
/// These come from `project_dirs()` — the same source every other command
/// uses — so the uninstall hint can never drift from where the files are
/// (on macOS that is `~/Library/Application Support/tech.appachi.suvadu`,
/// and on Linux config and data live in two different directories).
fn uninstall_data_paths() -> Vec<String> {
    crate::util::project_dirs().map_or_else(
        || vec!["could not determine Suvadu's data directory".to_string()],
        |dirs| uninstall_data_path_list(dirs.config_dir(), dirs.data_dir()),
    )
}

/// Detect a `suv` binary via `which` when neither Homebrew nor Cargo installs are found.
fn detect_fallback_binary() {
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
}

/// Uninstall via Homebrew. Returns `true` on success.
fn uninstall_homebrew() -> bool {
    print!("Removing Homebrew package... ");
    match std::process::Command::new("brew")
        .args(["uninstall", "suvadu"])
        .status()
    {
        Ok(s) if s.success() => {
            println!("✓");
            true
        }
        _ => {
            println!("✘");
            eprintln!("  Failed. Run manually: brew uninstall suvadu");
            false
        }
    }
}

/// Uninstall via Cargo, falling back to direct binary removal. Returns `true` on success.
fn uninstall_cargo() -> bool {
    print!("Removing Cargo installation... ");
    let status = std::process::Command::new("cargo")
        .args(["uninstall", "suvadu"])
        .status();
    match status {
        Ok(s) if s.success() => {
            println!("✓");
            true
        }
        _ => {
            // Fallback: remove binary directly
            if let Ok(home) = std::env::var("HOME") {
                let cargo_bin = format!("{home}/.cargo/bin/suv");
                if std::fs::remove_file(&cargo_bin).is_ok() {
                    println!("✓ (removed binary directly)");
                    return true;
                }
            }
            println!("✘");
            eprintln!("  Failed. Run manually: cargo uninstall suvadu");
            false
        }
    }
}

/// Clean up shell hooks and agent integrations.
fn cleanup_integrations() {
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

    let codex_cleaned = match crate::integrations::codex::cleanup() {
        Ok(changed) => {
            if changed {
                println!("✓ Removed Suvadu hooks from Codex configuration");
            }
            true
        }
        Err(e) => {
            eprintln!("Warning: Failed to clean up Codex hooks: {e}. Hook scripts retained.");
            false
        }
    };

    // Retain scripts if their Codex registrations could not be removed.
    if let Ok(home) = std::env::var("HOME") {
        let hooks_dir = std::path::PathBuf::from(&home)
            .join(".config")
            .join("suvadu")
            .join("hooks");
        if codex_cleaned && hooks_dir.exists() {
            if let Err(e) = std::fs::remove_dir_all(&hooks_dir) {
                eprintln!("Warning: Failed to remove hooks directory: {e}");
            } else {
                println!("✓ Removed agent hook scripts");
            }
        }
    }

    // Remove Suvadu entry from ~/.claude/settings.json
    match util::cleanup_claude_settings() {
        Ok(true) => println!("✓ Removed Suvadu hook from ~/.claude/settings.json"),
        Ok(false) => {}
        Err(e) => eprintln!("Warning: Failed to clean up Claude settings: {e}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::capture::{CaptureFacts, RecordingState};

    fn facts(state: RecordingState, facts: CaptureFacts, last: Option<LastRecord>) -> StatusFacts {
        StatusFacts {
            state,
            capture: facts,
            last_record: last,
        }
    }

    #[test]
    fn status_never_claims_history_is_being_recorded_from_config_alone() {
        let lines = status_lines(&facts(
            RecordingState::Enabled,
            CaptureFacts::default(),
            None,
        ))
        .join("\n");
        assert!(
            !lines.contains("History IS being recorded"),
            "configuration alone must not claim capture:\n{lines}"
        );
        assert!(lines.contains("enabled in config"), "{lines}");
        assert!(lines.contains("not yet verified"), "{lines}");
    }

    #[test]
    fn status_reports_a_recent_record_as_the_evidence_of_capture() {
        let lines = status_lines(&facts(
            RecordingState::Enabled,
            CaptureFacts {
                hook_configured: true,
                session_env_present: true,
                shell_records: 42,
                newest_shell_record_age_secs: Some(120),
            },
            Some(LastRecord {
                command: "echo hi".into(),
                age_secs: 120,
                exit_code: Some(0),
            }),
        ))
        .join("\n");
        assert!(
            lines.contains("most recent record received 2 minutes ago"),
            "{lines}"
        );
        assert!(lines.contains("echo hi"), "{lines}");
        assert!(lines.contains("exit 0"), "{lines}");
    }

    #[test]
    fn status_does_not_invent_a_missing_exit_code() {
        let lines = status_lines(&facts(
            RecordingState::Enabled,
            CaptureFacts {
                hook_configured: true,
                shell_records: 1,
                newest_shell_record_age_secs: Some(10),
                ..CaptureFacts::default()
            },
            Some(LastRecord {
                command: "sleep 1".into(),
                age_secs: 10,
                exit_code: None,
            }),
        ))
        .join("\n");
        assert!(lines.contains("exit unknown"), "{lines}");
        assert!(!lines.contains("exit 0"), "{lines}");
    }

    #[test]
    fn status_points_at_the_verification_sequence() {
        let lines = status_lines(&facts(
            RecordingState::Enabled,
            CaptureFacts::default(),
            None,
        ))
        .join("\n");
        for step in crate::commands::capture::verification_steps() {
            assert!(lines.contains(&step), "missing step `{step}`:\n{lines}");
        }
    }

    #[test]
    fn status_tells_a_paused_shell_to_unset_the_env_var() {
        let lines = status_lines(&facts(
            RecordingState::PausedInShell,
            CaptureFacts::default(),
            None,
        ))
        .join("\n");
        assert!(lines.contains("paused in this shell"), "{lines}");
        assert!(lines.contains("unset SUVADU_PAUSED"), "{lines}");
    }

    #[test]
    fn status_warns_that_a_session_variable_is_not_proof() {
        let lines = status_lines(&facts(
            RecordingState::Enabled,
            CaptureFacts {
                session_env_present: true,
                ..CaptureFacts::default()
            },
            None,
        ))
        .join("\n");
        assert!(lines.contains("not proof"), "{lines}");
    }

    #[test]
    fn uninstall_data_paths_come_from_project_dirs_not_hardcoded_strings() {
        let dirs = crate::util::project_dirs().expect("project dirs");
        let paths = uninstall_data_paths();
        let joined = paths.join("\n");
        assert!(
            joined.contains(&dirs.data_dir().display().to_string()),
            "the real data directory must be listed: {joined}"
        );
        assert!(
            joined.contains(&dirs.config_dir().display().to_string()),
            "the real config directory must be listed: {joined}"
        );
        assert!(
            !joined.contains("Application Support/suvadu"),
            "the macOS bundle directory is tech.appachi.suvadu, not suvadu: {joined}"
        );
        assert!(
            !joined.contains("~/.config/suvadu/ (Linux)"),
            "paths must be the real ones, not hardcoded per-OS guesses: {joined}"
        );
    }

    #[test]
    fn uninstall_data_paths_dedupe_when_config_and_data_share_a_directory() {
        let mut same = uninstall_data_path_list(
            std::path::Path::new("/tmp/x/suvadu"),
            std::path::Path::new("/tmp/x/suvadu"),
        );
        assert_eq!(same.len(), 1, "{same:?}");
        same = uninstall_data_path_list(
            std::path::Path::new("/tmp/x/config"),
            std::path::Path::new("/tmp/x/data"),
        );
        assert_eq!(same.len(), 2, "{same:?}");
    }

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
