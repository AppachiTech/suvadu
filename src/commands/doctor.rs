use std::path::{Path, PathBuf};

use crate::{config, db, models::SearchField, repository::Repository};

enum Status {
    Pass,
    Warn,
    Fail,
}

struct CheckResult {
    name: String,
    status: Status,
    detail: String,
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

    let mut results = vec![
        check_shell(),
        check_shell_hooks(),
        check_config(),
        check_database(),
        check_recording(),
    ];
    results.extend(check_mcp());
    results.push(check_agent_hooks());

    let (green, yellow, red) = if color {
        ("\x1b[32m", "\x1b[33m", "\x1b[31m")
    } else {
        ("", "", "")
    };
    let dim = if color { "\x1b[2m" } else { "" };

    for r in &results {
        let (icon, icon_color) = match r.status {
            Status::Pass => ("\u{2713}", green),
            Status::Warn => ("\u{26a0}", yellow),
            Status::Fail => ("\u{2717}", red),
        };

        // Pad name with dots to 20 chars
        let dots = ".".repeat(22_usize.saturating_sub(r.name.len()));
        println!(
            "  {}{} {}{dim}{dots}{reset} {icon_color}{icon}{reset} {}",
            bold, r.name, reset, r.detail
        );
    }

    let passed = results
        .iter()
        .filter(|r| matches!(r.status, Status::Pass))
        .count();
    let warnings = results
        .iter()
        .filter(|r| matches!(r.status, Status::Warn))
        .count();
    let failed = results
        .iter()
        .filter(|r| matches!(r.status, Status::Fail))
        .count();

    println!();
    println!(
        "  {green}{passed} passed{reset}, {yellow}{warnings} warnings{reset}, {red}{failed} failed{reset}"
    );
}

fn check_shell() -> CheckResult {
    let shell_path = std::env::var("SHELL").unwrap_or_default();
    let shell_name = shell_path
        .rsplit('/')
        .next()
        .unwrap_or("unknown")
        .to_string();

    if shell_name != "zsh" && shell_name != "bash" {
        return CheckResult {
            name: "Shell".to_string(),
            status: Status::Warn,
            detail: format!("{shell_name} (only zsh and bash are supported)"),
        };
    }

    // Get version
    let version_output = std::process::Command::new(&shell_path)
        .arg("--version")
        .output();

    let version_str = match version_output {
        Ok(output) => {
            let stdout = String::from_utf8_lossy(&output.stdout);
            let stderr = String::from_utf8_lossy(&output.stderr);
            let combined = format!("{stdout}{stderr}");
            extract_version(&combined, &shell_name)
        }
        Err(_) => None,
    };

    match version_str {
        Some((major, minor, display)) => {
            let (min_major, min_minor) = if shell_name == "zsh" { (5, 1) } else { (4, 0) };
            if major > min_major || (major == min_major && minor >= min_minor) {
                CheckResult {
                    name: "Shell".to_string(),
                    status: Status::Pass,
                    detail: format!("{shell_name} {display} (minimum: {min_major}.{min_minor})"),
                }
            } else {
                CheckResult {
                    name: "Shell".to_string(),
                    status: Status::Fail,
                    detail: format!(
                        "{shell_name} {display} is below minimum {min_major}.{min_minor}"
                    ),
                }
            }
        }
        None => CheckResult {
            name: "Shell".to_string(),
            status: Status::Pass,
            detail: format!("{shell_name} (version unknown)"),
        },
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
    let Ok(home) = std::env::var("HOME") else {
        return CheckResult {
            name: "Shell hooks".to_string(),
            status: Status::Warn,
            detail: "$HOME not set".to_string(),
        };
    };

    let shell_path = std::env::var("SHELL").unwrap_or_default();
    let shell_name = shell_path.rsplit('/').next().unwrap_or("unknown");

    let (rc_file, init_target) = match shell_name {
        "zsh" => (".zshrc", "zsh"),
        "bash" => (".bashrc", "bash"),
        _ => {
            return CheckResult {
                name: "Shell hooks".to_string(),
                status: Status::Warn,
                detail: format!("cannot check hooks for {shell_name}"),
            };
        }
    };

    let rc_path = PathBuf::from(&home).join(rc_file);
    if !rc_path.exists() {
        return CheckResult {
            name: "Shell hooks".to_string(),
            status: Status::Warn,
            detail: format!("~/{rc_file} not found"),
        };
    }

    std::fs::read_to_string(&rc_path).map_or_else(
        |_| CheckResult {
            name: "Shell hooks".to_string(),
            status: Status::Warn,
            detail: format!("cannot read ~/{rc_file}"),
        },
        |content| {
            if content.contains("suv init") {
                CheckResult {
                    name: "Shell hooks".to_string(),
                    status: Status::Pass,
                    detail: format!("found in ~/{rc_file}"),
                }
            } else {
                CheckResult {
                    name: "Shell hooks".to_string(),
                    status: Status::Fail,
                    detail: format!(
                        "not found in ~/{rc_file} (add: eval \"$(suv init {init_target})\")"
                    ),
                }
            }
        },
    )
}

fn check_config() -> CheckResult {
    let config_path = match config::get_config_path() {
        Ok(p) => p,
        Err(e) => {
            return CheckResult {
                name: "Config".to_string(),
                status: Status::Fail,
                detail: format!("cannot determine path: {e}"),
            };
        }
    };

    if !config_path.exists() {
        return CheckResult {
            name: "Config".to_string(),
            status: Status::Pass,
            detail: "using defaults (no config file)".to_string(),
        };
    }

    match config::load_config() {
        Ok(_) => {
            let display = abbreviate_home(&config_path);
            CheckResult {
                name: "Config".to_string(),
                status: Status::Pass,
                detail: format!("valid ({display})"),
            }
        }
        Err(e) => CheckResult {
            name: "Config".to_string(),
            status: Status::Fail,
            detail: format!("{e}"),
        },
    }
}

fn check_database() -> CheckResult {
    let db_path = match db::get_db_path() {
        Ok(p) => p,
        Err(e) => {
            return CheckResult {
                name: "Database".to_string(),
                status: Status::Fail,
                detail: format!("cannot determine path: {e}"),
            };
        }
    };

    if !db_path.exists() {
        return CheckResult {
            name: "Database".to_string(),
            status: Status::Warn,
            detail: "not found (run some commands first)".to_string(),
        };
    }

    let conn = match db::init_db(&db_path) {
        Ok(c) => c,
        Err(e) => {
            return CheckResult {
                name: "Database".to_string(),
                status: Status::Fail,
                detail: format!("cannot open: {e}"),
            };
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
        return CheckResult {
            name: "Database".to_string(),
            status: Status::Fail,
            detail: format!("integrity check failed: {integrity}"),
        };
    }

    // Entry count
    let repo = Repository::new(conn);
    let count = repo
        .count_filtered(&crate::repository::QueryFilter {
            after: None,
            before: None,
            tag_id: None,
            exit_code: None,
            query: None,
            prefix_match: false,
            executor: None,
            cwd: None,
            field: SearchField::Command,
            exclude_agents: false,
            cwd_prefix: false,
            failed_only: false,
        })
        .unwrap_or(0);

    CheckResult {
        name: "Database".to_string(),
        status: Status::Pass,
        detail: format!("healthy \u{2014} schema v{version}, {count} entries"),
    }
}

fn check_recording() -> CheckResult {
    let enabled = config::is_enabled().unwrap_or(false);
    let paused = config::is_paused();

    if enabled && !paused {
        CheckResult {
            name: "Recording".to_string(),
            status: Status::Pass,
            detail: "active".to_string(),
        }
    } else if enabled && paused {
        CheckResult {
            name: "Recording".to_string(),
            status: Status::Warn,
            detail: "paused (run: suv enable)".to_string(),
        }
    } else {
        CheckResult {
            name: "Recording".to_string(),
            status: Status::Fail,
            detail: "disabled (run: suv enable)".to_string(),
        }
    }
}

fn check_mcp() -> Vec<CheckResult> {
    let Ok(home) = std::env::var("HOME") else {
        return vec![];
    };

    let mut results = Vec::new();

    // Claude Code: ~/.claude.json
    let claude_path = PathBuf::from(&home).join(".claude.json");
    results.push(check_mcp_file(
        &claude_path,
        "MCP (Claude Code)",
        "suv init claude-code",
    ));

    // Cursor: ~/.cursor/mcp.json
    let cursor_path = PathBuf::from(&home).join(".cursor").join("mcp.json");
    results.push(check_mcp_file(
        &cursor_path,
        "MCP (Cursor)",
        "suv init cursor",
    ));

    results
}

fn check_mcp_file(path: &Path, name: &str, fix_cmd: &str) -> CheckResult {
    if !path.exists() {
        return CheckResult {
            name: name.to_string(),
            status: Status::Warn,
            detail: format!("not registered (run: {fix_cmd})"),
        };
    }

    std::fs::read_to_string(path).map_or_else(
        |_| CheckResult {
            name: name.to_string(),
            status: Status::Warn,
            detail: format!("cannot read {}", abbreviate_home(path)),
        },
        |content| {
            serde_json::from_str::<serde_json::Value>(&content).map_or_else(
                |_| CheckResult {
                    name: name.to_string(),
                    status: Status::Warn,
                    detail: format!("cannot parse {}", abbreviate_home(path)),
                },
                |json| {
                    let registered = json
                        .get("mcpServers")
                        .and_then(|s| s.get("suvadu"))
                        .is_some();
                    if registered {
                        let display = abbreviate_home(path);
                        CheckResult {
                            name: name.to_string(),
                            status: Status::Pass,
                            detail: format!("registered in {display}"),
                        }
                    } else {
                        CheckResult {
                            name: name.to_string(),
                            status: Status::Warn,
                            detail: format!("not registered (run: {fix_cmd})"),
                        }
                    }
                },
            )
        },
    )
}

fn check_agent_hooks() -> CheckResult {
    let Ok(home) = std::env::var("HOME") else {
        return CheckResult {
            name: "Agent hooks".to_string(),
            status: Status::Warn,
            detail: "$HOME not set".to_string(),
        };
    };

    // Hooks are installed to ~/.config/suvadu/hooks/ by `suv init claude-code` and `suv init cursor`
    let hooks_dir = PathBuf::from(home)
        .join(".config")
        .join("suvadu")
        .join("hooks");
    if !hooks_dir.exists() {
        return CheckResult {
            name: "Agent hooks".to_string(),
            status: Status::Warn,
            detail: "no hooks directory (run: suv init claude-code)".to_string(),
        };
    }

    let hook_files: Vec<String> = std::fs::read_dir(&hooks_dir)
        .ok()
        .map(|entries| {
            entries
                .filter_map(std::result::Result::ok)
                .map(|e| e.file_name().to_string_lossy().to_string())
                .collect()
        })
        .unwrap_or_default();

    if hook_files.is_empty() {
        CheckResult {
            name: "Agent hooks".to_string(),
            status: Status::Warn,
            detail: "hooks directory is empty".to_string(),
        }
    } else {
        CheckResult {
            name: "Agent hooks".to_string(),
            status: Status::Pass,
            detail: format!("{} hook script(s) installed", hook_files.len()),
        }
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
