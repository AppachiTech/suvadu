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

/// The newest stored shell record. Fields are reported exactly as stored —
/// a missing exit code stays missing — and `imported` says where the row
/// came from, because an imported row is history, not capture evidence.
pub struct LastRecord {
    pub command: String,
    pub age_secs: i64,
    pub exit_code: Option<i32>,
    pub imported: bool,
}

/// Everything `status_lines` needs, gathered by the caller so the rendering
/// itself stays pure and testable.
pub struct StatusFacts {
    pub state: capture::RecordingState,
    pub capture: capture::CaptureFacts,
    pub last_record: Option<LastRecord>,
    /// The shell being diagnosed, for the repair advice.
    pub shell_name: String,
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
        // Say which kind of row this is. Naming it "Last shell record"
        // without qualification was how an imported line read as capture.
        let label = if last.imported {
            "Last stored record (imported)"
        } else {
            "Last captured record"
        };
        lines.push(format!(
            "  {label}: {} \u{2014} {} ago, {exit}",
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

    let capture_fixes = capture::capture_fixes(&facts.capture, &facts.shell_name);
    if !capture_fixes.is_empty() {
        lines.push(String::new());
        lines.push("To start capturing:".to_string());
        for fix in capture_fixes {
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

pub fn handle_status() -> Result<(), Box<dyn std::error::Error>> {
    let global_enabled = config::is_enabled()?;
    let is_paused = config::is_paused();
    let state = capture::recording_state(global_enabled, is_paused);
    let session_id = capture::current_session_id();
    let hook_configured = capture::shell_hook_configured();
    let shell_name = capture::current_shell_name();

    // Open the database first: only stored records can prove capture.
    let db_path = db::get_db_path().ok();
    let repo = db_path
        .as_ref()
        .and_then(|p| db::init_db(p).ok())
        .map(Repository::new);

    // Rows are counted by provenance: an imported row is searchable history
    // and may carry any timestamp, so it can never be the thing that proves
    // a live hook is running.
    let record_stats = repo
        .as_ref()
        .and_then(|repo| repo.capture_record_stats(session_id.as_deref()).ok())
        .unwrap_or_default();
    let now_ms = chrono::Utc::now().timestamp_millis();
    let last_record = record_stats
        .newest_record
        .as_ref()
        .map(|record| LastRecord {
            command: record.command.clone(),
            age_secs: capture::age_secs(now_ms, record.started_at),
            exit_code: record.exit_code,
            imported: record.imported,
        });

    for line in status_lines(&StatusFacts {
        state,
        capture: capture::facts_from_records(
            hook_configured,
            session_id.is_some(),
            &record_stats,
            now_ms,
        ),
        last_record,
        shell_name,
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
            query_tokens: &[],
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

    let script_install = std::env::current_exe()
        .ok()
        .and_then(|exe| script_install_at(&exe));

    if !is_homebrew && !is_cargo && script_install.is_none() {
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
    if let Some(bin) = &script_install {
        println!("  • Install script ({})", bin.display());
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

    if let Some(bin) = &script_install {
        all_ok &= uninstall_script_install(bin);
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

/// The binary the install script put in place, if `exe` is one.
///
/// The script always links `suvadu` to `suv` in the same directory, and
/// nothing else does: a Homebrew or Cargo binary is recognised by its path
/// first, and a development build has no such link. This decides what
/// `suv uninstall` deletes, so anything short of that layout is `None`.
fn script_install_at(exe: &std::path::Path) -> Option<std::path::PathBuf> {
    let exe = exe.canonicalize().ok()?;
    let shown = exe.to_string_lossy();
    if crate::update::is_homebrew_path(&shown)
        || crate::update::is_cargo_path(&shown)
        || is_system_or_package_path(&shown)
    {
        return None;
    }
    let link = exe.parent()?.join("suvadu");
    let is_link = std::fs::symlink_metadata(&link)
        .ok()?
        .file_type()
        .is_symlink();
    (is_link && link.canonicalize().ok()? == exe).then_some(exe)
}

/// Whether `path` is somewhere the install script never installs but a
/// system package manager does: the Nix and Guix stores, Snap, `/opt/local`,
/// and the system's own binary directories. A package that happens to copy
/// the `suv` + `suvadu` layout there is that package manager's to remove.
fn is_system_or_package_path(path: &str) -> bool {
    const PREFIXES: &[&str] = &[
        "/nix/",
        "/gnu/store/",
        "/snap/",
        "/opt/local/",
        "/usr/bin/",
        "/usr/sbin/",
        "/bin/",
        "/sbin/",
    ];
    PREFIXES.iter().any(|prefix| path.starts_with(prefix))
}

/// Delete a script install's `suv` and the `suvadu` link beside it, and
/// nothing else. Files already gone are not an error.
fn remove_script_install_files(bin: &std::path::Path) -> std::io::Result<()> {
    for path in [bin.with_file_name("suvadu"), bin.to_path_buf()] {
        match std::fs::remove_file(&path) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e),
        }
    }
    Ok(())
}

/// Remove a script install. Tries as the current user first and uses sudo
/// only when the directory is not writable — the install script's own rule.
/// Returns `true` on success.
fn uninstall_script_install(bin: &std::path::Path) -> bool {
    print!("Removing {}... ", bin.display());
    let _ = std::io::Write::flush(&mut std::io::stdout());
    if remove_script_install_files(bin).is_ok() {
        println!("✓");
        return true;
    }
    println!("needs sudo");
    let link = bin.with_file_name("suvadu");
    let removed = std::process::Command::new("sudo")
        .arg("rm")
        .arg("-f")
        .arg(&link)
        .arg(bin)
        .status()
        .is_ok_and(|s| s.success());
    if removed {
        println!("  ✓ removed with sudo");
    } else {
        eprintln!(
            "  Failed. Run manually: sudo rm -f {} {}",
            link.display(),
            bin.display()
        );
    }
    removed
}

/// Detect a `suv` binary via `which` when no Homebrew, Cargo or install-script
/// install is found.
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
            shell_name: "zsh".to_string(),
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
                live_records: 42,
                newest_live_record_age_secs: Some(120),
                session_records: 42,
                newest_session_record_age_secs: Some(120),
                imported_records: 0,
            },
            Some(LastRecord {
                command: "echo hi".into(),
                age_secs: 120,
                exit_code: Some(0),
                imported: false,
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
                live_records: 1,
                newest_live_record_age_secs: Some(10),
                session_records: 1,
                newest_session_record_age_secs: Some(10),
                ..CaptureFacts::default()
            },
            Some(LastRecord {
                command: "sleep 1".into(),
                age_secs: 10,
                exit_code: None,
                imported: false,
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

    /// A directory laid out the way the install script leaves it.
    #[cfg(unix)]
    fn script_layout(dir: &std::path::Path) -> std::path::PathBuf {
        std::fs::create_dir_all(dir).unwrap();
        let bin = dir.join("suv");
        std::fs::write(&bin, "binary").unwrap();
        std::os::unix::fs::symlink(&bin, dir.join("suvadu")).unwrap();
        bin.canonicalize().unwrap()
    }

    #[cfg(unix)]
    #[test]
    fn a_script_install_is_recognised_by_its_suvadu_link() {
        let tmp = tempfile::tempdir().unwrap();
        let bin = script_layout(&tmp.path().join("bin"));

        assert_eq!(script_install_at(&bin), Some(bin.clone()));
        // Run as `suvadu`, it still names the real binary to remove.
        assert_eq!(
            script_install_at(&tmp.path().join("bin").join("suvadu")),
            Some(bin)
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_binary_without_the_install_scripts_link_is_not_a_script_install() {
        // A development build, or a copy placed by hand: nothing to delete.
        let tmp = tempfile::tempdir().unwrap();
        let bin = tmp.path().join("suv");
        std::fs::write(&bin, "binary").unwrap();
        assert_eq!(script_install_at(&bin), None);

        // A `suvadu` link that points somewhere else does not vouch for it.
        let other = tmp.path().join("other");
        std::fs::write(&other, "other").unwrap();
        std::os::unix::fs::symlink(&other, tmp.path().join("suvadu")).unwrap();
        assert_eq!(script_install_at(&bin), None);
    }

    #[cfg(unix)]
    #[test]
    fn package_manager_paths_are_never_treated_as_script_installs() {
        let tmp = tempfile::tempdir().unwrap();
        for dir in [".cargo/bin", "Cellar/suvadu/0.4.2/bin", "homebrew/bin"] {
            let bin = script_layout(&tmp.path().join(dir));
            assert_eq!(
                script_install_at(&bin),
                None,
                "{dir} belongs to a package manager"
            );
        }
    }

    #[test]
    fn system_and_package_locations_are_never_script_installs() {
        for path in [
            "/usr/bin/suv",
            "/bin/suv",
            "/usr/sbin/suv",
            "/nix/store/abc-suvadu-0.4.2/bin/suv",
            "/gnu/store/abc-suvadu/bin/suv",
            "/snap/suvadu/12/bin/suv",
            "/opt/local/bin/suv",
        ] {
            assert!(is_system_or_package_path(path), "{path}");
        }
        for path in [
            "/usr/local/bin/suv",
            "/home/u/.local/bin/suv",
            "/opt/tools/bin/suv",
        ] {
            assert!(!is_system_or_package_path(path), "{path}");
        }
    }

    #[cfg(unix)]
    #[test]
    fn removing_a_script_install_deletes_only_suv_and_its_link() {
        let tmp = tempfile::tempdir().unwrap();
        let bin = script_layout(tmp.path());
        let neighbour = tmp.path().join("other-tool");
        std::fs::write(&neighbour, "keep").unwrap();

        remove_script_install_files(&bin).unwrap();

        assert!(!bin.exists());
        assert!(std::fs::symlink_metadata(tmp.path().join("suvadu")).is_err());
        assert!(
            neighbour.exists(),
            "nothing else in the directory is touched"
        );
        // Already gone is not an error: a second run has nothing to do.
        remove_script_install_files(&bin).unwrap();
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
