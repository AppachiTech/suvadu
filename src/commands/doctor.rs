use std::path::{Path, PathBuf};

use crate::commands::capture;
use crate::integrations::registry::{AgentIntegration, InstallKind, REGISTRY};
use crate::{config, db, models::SearchField, repository::Repository};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Status {
    Pass,
    Warn,
    Fail,
    /// An optional feature that is simply not in use. Deliberately distinct
    /// from a warning: an integration nobody installed is not a defect.
    NotConfigured,
}

/// Whether a check blocks Suvadu's core promise (recording shell history) or
/// covers an optional integration the user may never want.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Category {
    Blocker,
    Optional,
}

struct CheckResult {
    name: String,
    status: Status,
    detail: String,
    /// The exact repair for this check, or `None` when nothing is wrong (or
    /// nothing *needs* fixing, as for an unused optional integration).
    fix: Option<String>,
    category: Category,
    /// Per-aspect breakdown printed under an agent row.
    aspects: Option<String>,
}

impl CheckResult {
    fn new(name: &str, status: Status, detail: String, category: Category) -> Self {
        Self {
            name: name.to_string(),
            status,
            detail,
            fix: None,
            category,
            aspects: None,
        }
    }

    fn blocker(name: &str, status: Status, detail: String) -> Self {
        Self::new(name, status, detail, Category::Blocker)
    }

    fn with_fix(mut self, fix: impl Into<String>) -> Self {
        self.fix = Some(fix.into());
        self
    }

    fn with_aspects(mut self, aspects: String) -> Self {
        self.aspects = Some(aspects);
        self
    }
}

pub fn handle_doctor() {
    let color = crate::util::color_enabled();
    let (bold, reset) = if color {
        ("\x1b[1m", "\x1b[0m")
    } else {
        ("", "")
    };

    println!("{bold}Suvadu Doctor{reset}");
    println!();

    // Diagnostics only read: never create the database as a side effect of
    // running `suv doctor`, or a clean machine stops looking clean.
    let repo = db::get_db_path()
        .ok()
        .filter(|path| path.exists())
        .and_then(|path| db::init_db(&path).ok())
        .map(Repository::new);

    let mut checks = vec![
        check_shell(),
        check_shell_hooks(),
        check_config(),
        check_database(),
        check_recording(),
        assess_capture(&gather_capture_facts(repo.as_ref())),
    ];
    checks.extend(check_agents(repo.as_ref()));

    print_report(&checks, &storage_lines(repo.as_ref()), color);
}

/// Storage usage by category, or nothing at all when there is no database to
/// measure. Read-only: it never creates the database or the backup directory.
fn storage_lines(repo: Option<&Repository>) -> Vec<String> {
    let Some(repo) = repo else {
        return Vec::new();
    };
    let backups = db::backup_dir_path().ok();
    repo.storage_usage(backups.as_deref())
        .map(|usage| usage.report_lines())
        .unwrap_or_default()
}

fn print_report(checks: &[CheckResult], storage: &[String], color: bool) {
    let (bold, reset) = if color {
        ("\x1b[1m", "\x1b[0m")
    } else {
        ("", "")
    };
    let (green, yellow, red) = if color {
        ("\x1b[32m", "\x1b[33m", "\x1b[31m")
    } else {
        ("", "", "")
    };
    let dim = if color { "\x1b[2m" } else { "" };

    let sections = [
        (Category::Blocker, "Required for shell history capture", ""),
        (
            Category::Optional,
            "Optional integrations",
            "only the ones you actually use need to pass",
        ),
    ];

    for (category, title, note) in sections {
        let rows: Vec<&CheckResult> = checks.iter().filter(|c| c.category == category).collect();
        if rows.is_empty() {
            continue;
        }
        if note.is_empty() {
            println!("{bold}{title}{reset}");
        } else {
            println!("{bold}{title}{reset} {dim}({note}){reset}");
        }
        for r in rows {
            let (icon, icon_color) = match r.status {
                Status::Pass => ("\u{2713}", green),
                Status::Warn => ("\u{26a0}", yellow),
                Status::Fail => ("\u{2717}", red),
                Status::NotConfigured => ("\u{25cb}", dim),
            };
            let dots = ".".repeat(22_usize.saturating_sub(r.name.len()));
            println!(
                "  {bold}{}{reset} {dim}{dots}{reset} {icon_color}{icon}{reset} {}",
                r.name, r.detail
            );
            if let Some(aspects) = &r.aspects {
                println!("      {dim}{aspects}{reset}");
            }
        }
        println!();
    }

    let repairs = repairs(checks);
    if !repairs.is_empty() {
        println!("{bold}Repairs{reset}");
        for repair in repairs {
            println!("  {repair}");
        }
        println!();
    }

    if !storage.is_empty() {
        println!("{bold}Storage{reset} {dim}(what is stored, and what makes it go away){reset}");
        for line in storage {
            println!("  {line}");
        }
        println!();
    }

    println!("{bold}Verify capture end-to-end{reset}");
    for step in capture::verification_steps() {
        println!("  {step}");
    }
    println!();

    let (passed, warnings, failed, not_configured) = summarize(checks);
    println!(
        "  {green}{passed} passed{reset}, {yellow}{warnings} warnings{reset}, {red}{failed} failed{reset}, {dim}{not_configured} not configured (optional){reset}"
    );
}

/// Count checks by status: (passed, warnings, failed, not configured).
fn summarize(checks: &[CheckResult]) -> (usize, usize, usize, usize) {
    let count = |want: Status| checks.iter().filter(|c| c.status == want).count();
    (
        count(Status::Pass),
        count(Status::Warn),
        count(Status::Fail),
        count(Status::NotConfigured),
    )
}

/// One repair line per check that has something to repair.
fn repairs(checks: &[CheckResult]) -> Vec<String> {
    checks
        .iter()
        .filter_map(|c| c.fix.as_ref().map(|fix| format!("{}: {fix}", c.name)))
        .collect()
}

fn check_shell() -> CheckResult {
    let shell_path = std::env::var("SHELL").unwrap_or_default();
    let shell_name = capture::current_shell_name();

    let version = std::process::Command::new(&shell_path)
        .arg("--version")
        .output()
        .ok()
        .and_then(|output| {
            let stdout = String::from_utf8_lossy(&output.stdout);
            let stderr = String::from_utf8_lossy(&output.stderr);
            extract_version(&format!("{stdout}{stderr}"), &shell_name)
        });

    assess_shell(&shell_name, version)
}

/// Judge the interactive shell. An unsupported shell gets its own explanation:
/// it blocks *shell* capture only, and agent capture is unaffected.
fn assess_shell(shell_name: &str, version: Option<(u32, u32, String)>) -> CheckResult {
    if capture::rc_file_for_shell(shell_name).is_none() {
        return CheckResult::blocker(
            "Shell",
            Status::Warn,
            format!("{shell_name} \u{2014} shell capture supports zsh and bash only"),
        )
        .with_fix(
            "run an interactive zsh or bash for shell capture; agent capture does not depend on your shell",
        );
    }

    let (min_major, min_minor) = if shell_name == "zsh" { (5, 1) } else { (4, 0) };
    match version {
        None => CheckResult::blocker(
            "Shell",
            Status::Pass,
            format!("{shell_name} (version unknown)"),
        ),
        Some((major, minor, display)) => {
            if major > min_major || (major == min_major && minor >= min_minor) {
                CheckResult::blocker(
                    "Shell",
                    Status::Pass,
                    format!("{shell_name} {display} (minimum: {min_major}.{min_minor})"),
                )
            } else {
                CheckResult::blocker(
                    "Shell",
                    Status::Fail,
                    format!("{shell_name} {display} is below minimum {min_major}.{min_minor}"),
                )
                .with_fix(format!(
                    "upgrade {shell_name} to {min_major}.{min_minor} or newer, or use a supported shell"
                ))
            }
        }
    }
}

/// Extract major.minor version from shell --version output.
fn extract_version(output: &str, shell: &str) -> Option<(u32, u32, String)> {
    // zsh: "zsh 5.9 (x86_64-apple-darwin24.0)"
    // bash: "GNU bash, version 5.2.37(1)-release ..."
    let pattern = if shell == "zsh" {
        r"zsh\s+(\d+)\.(\d+)"
    } else {
        r"version\s+(\d+)\.(\d+)"
    };
    let re = regex::Regex::new(pattern).ok()?;
    let caps = re.captures(output)?;
    let major: u32 = caps.get(1)?.as_str().parse().ok()?;
    let minor: u32 = caps.get(2)?.as_str().parse().ok()?;
    Some((major, minor, format!("{major}.{minor}")))
}

fn check_shell_hooks() -> CheckResult {
    let shell_name = capture::current_shell_name();
    let Ok(home) = std::env::var("HOME") else {
        return CheckResult::blocker("Shell hooks", Status::Warn, "$HOME not set".to_string())
            .with_fix("set $HOME so Suvadu can find your shell's rc file");
    };
    let contents = capture::rc_file_for_shell(&shell_name)
        .and_then(|rc| std::fs::read_to_string(PathBuf::from(&home).join(rc)).ok());
    assess_shell_hooks(&shell_name, contents.as_deref())
}

/// Judge the rc file. A missing rc file and an rc file without the hook line
/// are different problems, so they get different explanations and repairs.
fn assess_shell_hooks(shell_name: &str, rc_contents: Option<&str>) -> CheckResult {
    let Some(rc_file) = capture::rc_file_for_shell(shell_name) else {
        return CheckResult::blocker(
            "Shell hooks",
            Status::Warn,
            format!("cannot check hooks for {shell_name}"),
        )
        .with_fix("shell hooks are generated for zsh and bash only");
    };
    let eval_line = format!("eval \"$(suv init {shell_name})\"");
    match rc_contents {
        None => CheckResult::blocker(
            "Shell hooks",
            Status::Warn,
            format!("~/{rc_file} not found or unreadable"),
        )
        .with_fix(format!(
            "create ~/{rc_file} containing: {eval_line}, then start a new shell"
        )),
        Some(contents) if capture::rc_hook_configured(contents) => {
            CheckResult::blocker("Shell hooks", Status::Pass, format!("found in ~/{rc_file}"))
        }
        Some(_) => CheckResult::blocker(
            "Shell hooks",
            Status::Fail,
            format!("no `suv init` line in ~/{rc_file}"),
        )
        .with_fix(format!(
            "add to ~/{rc_file}: {eval_line}, then start a new shell"
        )),
    }
}

fn check_config() -> CheckResult {
    let config_path = match config::get_config_path() {
        Ok(p) => p,
        Err(e) => {
            return CheckResult::blocker(
                "Config",
                Status::Fail,
                format!("cannot determine path: {e}"),
            )
            .with_fix("check that your home directory is writable");
        }
    };

    if !config_path.exists() {
        return CheckResult::blocker(
            "Config",
            Status::Pass,
            "using defaults (no config file)".to_string(),
        );
    }

    match config::load_config() {
        Ok(_) => {
            let display = abbreviate_home(&config_path);
            CheckResult::blocker("Config", Status::Pass, format!("valid ({display})"))
        }
        Err(e) => CheckResult::blocker("Config", Status::Fail, format!("{e}")).with_fix(format!(
            "fix or delete {} to fall back to defaults",
            abbreviate_home(&config_path)
        )),
    }
}

fn check_database() -> CheckResult {
    let db_path = match db::get_db_path() {
        Ok(p) => p,
        Err(e) => {
            return CheckResult::blocker(
                "Database",
                Status::Fail,
                format!("cannot determine path: {e}"),
            )
            .with_fix("check that your data directory is writable");
        }
    };

    if !db_path.exists() {
        return CheckResult::blocker(
            "Database",
            Status::Warn,
            "not created yet (no command has been recorded)".to_string(),
        )
        .with_fix("run the verification sequence below to create it");
    }

    let conn = match db::init_db(&db_path) {
        Ok(c) => c,
        Err(e) => {
            return CheckResult::blocker("Database", Status::Fail, format!("cannot open: {e}"))
                .with_fix(format!(
                    "check permissions on {}, or restore a backup with suv backup",
                    abbreviate_home(&db_path)
                ));
        }
    };

    // Schema version
    let version: i64 = conn
        .query_row("SELECT version FROM schema_version LIMIT 1", [], |row| {
            row.get(0)
        })
        .unwrap_or(0);

    // Integrity check
    let integrity: String = conn
        .query_row("PRAGMA integrity_check", [], |row| row.get(0))
        .unwrap_or_else(|_| "error".to_string());

    if integrity != "ok" {
        return CheckResult::blocker(
            "Database",
            Status::Fail,
            format!("integrity check failed: {integrity}"),
        )
        .with_fix("restore the newest file from the backups directory next to the database");
    }

    // Entry count
    let repo = Repository::new(conn);
    let count = repo
        .count_filtered(&crate::repository::QueryFilter {
            field: SearchField::Command,
            ..Default::default()
        })
        .unwrap_or(0);

    CheckResult::blocker(
        "Database",
        Status::Pass,
        format!("healthy \u{2014} schema v{version}, {count} entries"),
    )
}

fn check_recording() -> CheckResult {
    assess_recording(config::is_enabled().unwrap_or(false), config::is_paused())
}

/// Judge the recording configuration.
///
/// "Disabled in config" and "paused in this shell" are separate states with
/// separate repairs: `suv enable` rewrites the config file and cannot clear
/// the `SUVADU_PAUSED` variable that `eval $(suv pause)` exported here.
fn assess_recording(config_enabled: bool, paused_in_shell: bool) -> CheckResult {
    let state = capture::recording_state(config_enabled, paused_in_shell);
    let status = match state {
        capture::RecordingState::Enabled => Status::Pass,
        capture::RecordingState::PausedInShell => Status::Warn,
        _ => Status::Fail,
    };
    let check = CheckResult::blocker("Recording", status, state.label().to_string());
    let fixes = capture::recording_fixes(state);
    if fixes.is_empty() {
        check
    } else {
        check.with_fix(fixes.join("; "))
    }
}

/// Gather the stored evidence that shell capture actually works.
fn gather_capture_facts(repo: Option<&Repository>) -> capture::CaptureFacts {
    let mut facts = capture::CaptureFacts {
        hook_configured: capture::shell_hook_configured(),
        session_env_present: std::env::var("SUVADU_SESSION_ID").is_ok(),
        ..Default::default()
    };
    if let Some(repo) = repo {
        let filter = crate::repository::QueryFilter {
            exclude_agents: true,
            ..Default::default()
        };
        facts.shell_records = repo.count_filtered(&filter).unwrap_or(0);
        facts.newest_shell_record_age_secs = repo
            .get_recent_entries(1, 0, &filter, None)
            .ok()
            .and_then(|entries| entries.into_iter().next())
            .map(|entry| (chrono::Utc::now().timestamp_millis() - entry.started_at).max(0) / 1000);
    }
    facts
}

/// Report what the stored data proves, separately from what the config allows.
fn assess_capture(facts: &capture::CaptureFacts) -> CheckResult {
    let evidence = capture::capture_evidence(facts);
    let status = if evidence.is_proven() {
        Status::Pass
    } else {
        Status::Warn
    };
    let check = CheckResult::blocker("Capture evidence", status, evidence.headline());
    if evidence.is_proven() {
        check
    } else {
        check.with_fix("run the verification sequence below and re-run suv doctor")
    }
}

/// How an agent integration is installed on this machine.
#[derive(Debug, Clone, PartialEq, Eq)]
enum IntegrationInstall {
    Installed,
    /// Installed, but its hook script cannot work (missing binary, stale
    /// metadata, not executable). The string is the exact reason.
    Broken(String),
    NotInstalled,
    /// Nothing to install: the agent's terminal is covered by shell hooks.
    ShellHookBased,
}

/// Whether Suvadu's MCP server is registered with an agent.
#[derive(Debug, Clone, PartialEq, Eq)]
enum McpState {
    Registered,
    NotRegistered,
    Unreadable(String),
    /// This agent has no MCP support in Suvadu.
    Unsupported,
}

/// The five independent facts `suv doctor` reports per agent.
#[derive(Debug, Clone, PartialEq, Eq)]
struct AgentEvidence {
    process_detected: bool,
    integration: IntegrationInstall,
    commands_captured: i64,
    /// `None` when the agent has no native session capture at all.
    native_sessions: Option<i64>,
    mcp: McpState,
}

/// Judge one agent integration.
///
/// An integration nobody installed and nobody is running is reported as "not
/// in use", never as a failure: not using Codex is not a broken installation.
fn assess_agent(agent: &AgentIntegration, ev: &AgentEvidence) -> CheckResult {
    let aspects = format!(
        "process: {} \u{b7} integration: {} \u{b7} commands: {} \u{b7} native sessions: {} \u{b7} MCP: {}",
        if ev.process_detected {
            "detected"
        } else {
            "not detected"
        },
        match &ev.integration {
            IntegrationInstall::Installed => "installed",
            IntegrationInstall::Broken(_) => "installed but broken",
            IntegrationInstall::NotInstalled => "not installed",
            IntegrationInstall::ShellHookBased => "shell hooks (nothing to install)",
        },
        ev.commands_captured,
        ev.native_sessions
            .map_or_else(|| "not supported".to_string(), |n| n.to_string()),
        match &ev.mcp {
            McpState::Registered => "registered".to_string(),
            McpState::NotRegistered => "not registered".to_string(),
            McpState::Unreadable(why) => format!("cannot read config ({why})"),
            McpState::Unsupported => "not supported".to_string(),
        },
    );

    let optional = |status: Status, detail: String| {
        CheckResult::new(agent.display_name, status, detail, Category::Optional)
    };

    match &ev.integration {
        IntegrationInstall::Broken(reason) => optional(
            Status::Fail,
            format!("integration installed but broken: {reason}"),
        )
        .with_fix(format!("re-run {}", agent.init_hint()))
        .with_aspects(aspects),

        _ if ev.commands_captured > 0 => optional(
            Status::Pass,
            format!("in use \u{2014} {} commands captured", ev.commands_captured),
        )
        .with_aspects(aspects),

        IntegrationInstall::Installed => optional(
            Status::Warn,
            "integration installed, nothing captured yet".to_string(),
        )
        .with_fix(format!(
            "run a command inside {} and re-run suv doctor",
            agent.display_name
        ))
        .with_aspects(aspects),

        IntegrationInstall::ShellHookBased if ev.process_detected => {
            optional(Status::Warn, "running, nothing captured yet".to_string())
                .with_fix(format!(
                    "open a new terminal inside {} (its commands are captured by the shell hooks)",
                    agent.display_name
                ))
                .with_aspects(aspects)
        }

        IntegrationInstall::NotInstalled if ev.process_detected => optional(
            Status::Warn,
            "running, but the integration is not installed".to_string(),
        )
        .with_fix(format!("run {} to capture its commands", agent.init_hint()))
        .with_aspects(aspects),

        _ => optional(
            Status::NotConfigured,
            format!("not in use (optional: {})", agent.init_hint()),
        ),
    }
}

fn check_agents(repo: Option<&Repository>) -> Vec<CheckResult> {
    let home = std::env::var("HOME").map(PathBuf::from).ok();
    let processes = running_processes();
    let hooks = installed_hook_scripts(home.as_deref());

    REGISTRY
        .iter()
        .map(|agent| {
            let ev = AgentEvidence {
                process_detected: process_detected(agent.process_names, &processes),
                integration: integration_install(agent, home.as_deref(), &hooks),
                commands_captured: repo.map_or(0, |r| {
                    r.count_entries_by_executor(agent.executor_name)
                        .unwrap_or(0)
                }),
                native_sessions: agent.session_agent.map(|name| {
                    repo.map_or(0, |r| r.count_ai_sessions_by_agent(name).unwrap_or(0))
                }),
                mcp: mcp_state(agent, home.as_deref()),
            };
            assess_agent(agent, &ev)
        })
        .collect()
}

/// Base names of running processes, one per line. Read-only: `ps` output is
/// only ever inspected, never executed.
fn running_processes() -> String {
    std::process::Command::new("ps")
        .args(["-A", "-o", "comm="])
        .output()
        .ok()
        .map(|o| String::from_utf8_lossy(&o.stdout).to_string())
        .unwrap_or_default()
}

/// `true` if any running process's base name is exactly one of `names`.
/// Exact matching keeps an unrelated `some-codex-wrapper` from implying Codex.
fn process_detected(names: &[&str], ps_output: &str) -> bool {
    ps_output.lines().any(|line| {
        let base = line.trim().rsplit('/').next().unwrap_or("");
        names.contains(&base)
    })
}

/// Every Suvadu hook script on disk, as (filename, inspection result).
fn installed_hook_scripts(home: Option<&Path>) -> Vec<(String, Result<(), String>)> {
    let Some(home) = home else {
        return vec![];
    };
    let Ok(entries) = std::fs::read_dir(home.join(".config/suvadu/hooks")) else {
        return vec![];
    };
    let mut scripts: Vec<(String, Result<(), String>)> = entries
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "sh"))
        .map(|path| {
            let name = path
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .to_string();
            (name, crate::integrations::agent_hook::inspect(&path))
        })
        .collect();
    scripts.sort_by(|a, b| a.0.cmp(&b.0));
    scripts
}

fn integration_install(
    agent: &AgentIntegration,
    home: Option<&Path>,
    hooks: &[(String, Result<(), String>)],
) -> IntegrationInstall {
    match agent.install {
        InstallKind::ShellHooks => IntegrationInstall::ShellHookBased,
        InstallKind::PluginFile(relpath) => {
            if home.is_some_and(|home| home.join(relpath).exists()) {
                IntegrationInstall::Installed
            } else {
                IntegrationInstall::NotInstalled
            }
        }
        InstallKind::HookScripts => {
            let mine: Vec<&(String, Result<(), String>)> = hooks
                .iter()
                .filter(|(name, _)| {
                    crate::integrations::registry::find_by_hook_filename(name)
                        .is_some_and(|owner| owner.id == agent.id)
                })
                .collect();
            if mine.is_empty() {
                return IntegrationInstall::NotInstalled;
            }
            mine.iter()
                .find_map(|(name, result)| {
                    result
                        .as_ref()
                        .err()
                        .map(|reason| IntegrationInstall::Broken(format!("{name}: {reason}")))
                })
                .unwrap_or(IntegrationInstall::Installed)
        }
    }
}

fn mcp_state(agent: &AgentIntegration, home: Option<&Path>) -> McpState {
    let (Some(relpath), Some(home)) = (agent.mcp_config_relpath, home) else {
        return McpState::Unsupported;
    };
    let path = home.join(relpath);
    if !path.exists() {
        return McpState::NotRegistered;
    }
    match std::fs::read_to_string(&path) {
        Err(e) => McpState::Unreadable(e.to_string()),
        Ok(contents) => serde_json::from_str::<serde_json::Value>(&contents).map_or_else(
            |_| McpState::Unreadable(format!("invalid JSON in {}", abbreviate_home(&path))),
            |json| {
                if json
                    .get("mcpServers")
                    .and_then(|s| s.get("suvadu"))
                    .is_some()
                {
                    McpState::Registered
                } else {
                    McpState::NotRegistered
                }
            },
        ),
    }
}

/// Replace $HOME prefix with ~ for display.
fn abbreviate_home(path: &Path) -> String {
    if let Ok(home) = std::env::var("HOME") {
        let s = path.display().to_string();
        if let Some(rest) = s.strip_prefix(&home) {
            return format!("~{rest}");
        }
    }
    path.display().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::integrations::registry::REGISTRY;

    fn agent(id: &str) -> &'static crate::integrations::registry::AgentIntegration {
        REGISTRY.iter().find(|a| a.id == id).unwrap()
    }

    fn unused() -> AgentEvidence {
        AgentEvidence {
            process_detected: false,
            integration: IntegrationInstall::NotInstalled,
            commands_captured: 0,
            native_sessions: None,
            mcp: McpState::NotRegistered,
        }
    }

    #[test]
    fn paused_recording_is_repaired_by_unsetting_the_env_var() {
        let r = assess_recording(true, true);
        assert!(matches!(r.status, Status::Warn), "{}", r.detail);
        assert!(r.detail.contains("paused in this shell"), "{}", r.detail);
        let fix = r.fix.unwrap_or_default();
        assert!(fix.contains("unset SUVADU_PAUSED"), "{fix}");
        assert!(
            !fix.contains("suv enable"),
            "`suv enable` cannot clear SUVADU_PAUSED: {fix}"
        );
    }

    #[test]
    fn disabled_recording_is_repaired_by_suv_enable() {
        let r = assess_recording(false, false);
        assert!(matches!(r.status, Status::Fail));
        assert!(r.detail.contains("disabled in config"), "{}", r.detail);
        let fix = r.fix.unwrap_or_default();
        assert!(fix.contains("suv enable"), "{fix}");
        assert!(!fix.contains("unset SUVADU_PAUSED"), "{fix}");
    }

    #[test]
    fn disabled_and_paused_lists_both_repairs() {
        let fix = assess_recording(false, true).fix.unwrap_or_default();
        assert!(fix.contains("suv enable"), "{fix}");
        assert!(fix.contains("unset SUVADU_PAUSED"), "{fix}");
    }

    #[test]
    fn recording_is_a_blocker_and_agents_are_optional() {
        assert!(matches!(
            assess_recording(true, false).category,
            Category::Blocker
        ));
        assert!(matches!(
            assess_agent(agent("codex"), &unused()).category,
            Category::Optional
        ));
    }

    #[test]
    fn an_unused_integration_is_not_reported_as_broken() {
        let r = assess_agent(agent("codex"), &unused());
        assert!(
            matches!(r.status, Status::NotConfigured),
            "unused integrations must not look broken: {}",
            r.detail
        );
        assert!(r.fix.is_none(), "no repair is needed: {:?}", r.fix);
        assert!(r.detail.contains("not in use"), "{}", r.detail);
        let (_, _, failed, not_configured) = summarize(&[assess_agent(agent("codex"), &unused())]);
        assert_eq!((failed, not_configured), (0, 1));
    }

    #[test]
    fn a_running_agent_without_the_integration_is_a_warning_with_an_init_repair() {
        let r = assess_agent(
            agent("codex"),
            &AgentEvidence {
                process_detected: true,
                ..unused()
            },
        );
        assert!(matches!(r.status, Status::Warn), "{}", r.detail);
        assert!(r.fix.unwrap_or_default().contains("suv init codex"));
    }

    #[test]
    fn a_broken_hook_script_reports_the_reason_and_how_to_repair_it() {
        let r = assess_agent(
            agent("claude-code"),
            &AgentEvidence {
                process_detected: true,
                integration: IntegrationInstall::Broken(
                    "saved binary is missing and suv is not on PATH".into(),
                ),
                ..unused()
            },
        );
        assert!(matches!(r.status, Status::Fail));
        assert!(r.detail.contains("saved binary is missing"), "{}", r.detail);
        assert!(r.fix.unwrap_or_default().contains("suv init claude-code"));
    }

    #[test]
    fn an_installed_integration_without_captures_is_unverified_not_healthy() {
        let r = assess_agent(
            agent("claude-code"),
            &AgentEvidence {
                process_detected: true,
                integration: IntegrationInstall::Installed,
                ..unused()
            },
        );
        assert!(matches!(r.status, Status::Warn), "{}", r.detail);
        assert!(r.detail.contains("nothing captured yet"), "{}", r.detail);
    }

    #[test]
    fn agent_aspects_distinguish_process_integration_commands_sessions_and_mcp() {
        let r = assess_agent(
            agent("claude-code"),
            &AgentEvidence {
                process_detected: true,
                integration: IntegrationInstall::Installed,
                commands_captured: 42,
                native_sessions: Some(3),
                mcp: McpState::Registered,
            },
        );
        assert!(matches!(r.status, Status::Pass), "{}", r.detail);
        let aspects = r.aspects.unwrap_or_default();
        assert!(aspects.contains("process: detected"), "{aspects}");
        assert!(aspects.contains("integration: installed"), "{aspects}");
        assert!(aspects.contains("commands: 42"), "{aspects}");
        assert!(aspects.contains("native sessions: 3"), "{aspects}");
        assert!(aspects.contains("MCP: registered"), "{aspects}");
    }

    #[test]
    fn agents_without_native_sessions_or_mcp_say_so_instead_of_reporting_zero() {
        let r = assess_agent(
            agent("cursor"),
            &AgentEvidence {
                process_detected: true,
                integration: IntegrationInstall::Installed,
                commands_captured: 5,
                native_sessions: None,
                mcp: McpState::Unsupported,
            },
        );
        let aspects = r.aspects.unwrap_or_default();
        assert!(
            aspects.contains("native sessions: not supported"),
            "{aspects}"
        );
        assert!(aspects.contains("MCP: not supported"), "{aspects}");
    }

    #[test]
    fn shell_hook_based_integrations_are_not_asked_to_install_hook_scripts() {
        let r = assess_agent(
            agent("antigravity"),
            &AgentEvidence {
                process_detected: true,
                integration: IntegrationInstall::ShellHookBased,
                commands_captured: 7,
                native_sessions: None,
                mcp: McpState::Unsupported,
            },
        );
        assert!(matches!(r.status, Status::Pass), "{}", r.detail);
        let aspects = r.aspects.unwrap_or_default();
        assert!(aspects.contains("shell hooks"), "{aspects}");
    }

    #[test]
    fn an_unsupported_shell_has_its_own_explanation() {
        let r = assess_shell("fish", None);
        assert!(r.detail.contains("fish"), "{}", r.detail);
        assert!(r.detail.contains("zsh"), "{}", r.detail);
        let fix = r.fix.unwrap_or_default();
        assert!(fix.contains("agent"), "agent capture still works: {fix}");
    }

    #[test]
    fn a_missing_rc_file_and_a_missing_hook_line_are_different_problems() {
        let missing_rc = assess_shell_hooks("zsh", None);
        assert!(
            missing_rc.detail.contains("not found"),
            "{}",
            missing_rc.detail
        );
        let no_hook = assess_shell_hooks("zsh", Some("# nothing here\n"));
        assert!(matches!(no_hook.status, Status::Fail));
        assert!(
            no_hook.fix.unwrap_or_default().contains("suv init zsh"),
            "the repair must be the exact eval line"
        );
        let installed = assess_shell_hooks("zsh", Some("eval \"$(suv init zsh)\"\n"));
        assert!(matches!(installed.status, Status::Pass));
        assert!(installed.fix.is_none());
    }

    #[test]
    fn capture_evidence_is_a_separate_blocker_check_from_recording() {
        let unverified = assess_capture(&crate::commands::capture::CaptureFacts::default());
        assert!(
            matches!(unverified.status, Status::Warn),
            "{}",
            unverified.detail
        );
        assert!(
            unverified.detail.contains("not yet verified"),
            "{}",
            unverified.detail
        );
        assert!(matches!(unverified.category, Category::Blocker));

        let proven = assess_capture(&crate::commands::capture::CaptureFacts {
            hook_configured: true,
            shell_records: 3,
            newest_shell_record_age_secs: Some(60),
            ..Default::default()
        });
        assert!(matches!(proven.status, Status::Pass), "{}", proven.detail);
    }

    #[test]
    fn processes_match_by_exact_basename() {
        let ps = "/usr/bin/zsh\n/opt/homebrew/bin/codex\nsome-codex-wrapper\n";
        assert!(process_detected(&["codex"], ps));
        assert!(!process_detected(&["claude"], ps));
        // A different program that merely contains the name must not count.
        assert!(!process_detected(&["wrapper"], ps));
    }

    #[test]
    fn repairs_are_listed_per_failing_check_only() {
        let checks = vec![
            assess_recording(true, true),
            assess_agent(agent("codex"), &unused()),
        ];
        let repairs = repairs(&checks);
        assert_eq!(repairs.len(), 1, "{repairs:?}");
        assert!(
            repairs[0].contains("unset SUVADU_PAUSED"),
            "{:?}",
            repairs[0]
        );
    }
}
