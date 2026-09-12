//! Upgrade reminders without modifying agent configuration or protocol output.
use serde_json::Value;
use std::io::IsTerminal;
use std::path::PathBuf;

pub const CODEX_SETUP: &str = "Codex session capture: run `suv init codex` with the updated binary.\n  In the Codex terminal CLI, open /hooks and review/trust Suvadu's Stop and SessionEnd hooks.\n  Then relaunch Codex; for the VS Code extension, fully quit and reopen VS Code.";

pub const AGENT_UPGRADE: &str = "After updating agent integrations:\n  Codex: run `suv init codex`, then open /hooks in the Codex terminal CLI and review/trust Suvadu hooks.\n  Relaunch Codex; for its VS Code extension, fully quit and reopen VS Code.\n  Claude Code: after refreshing hooks with `suv init claude-code`, relaunch Claude Code (or its VS Code host).\n  This release's native session/token capture supports Codex; Claude capture is planned.";

fn needs_refresh(hooks: &Value, script: &str) -> bool {
    let quoted = crate::integrations::shell_escape(script);
    let registered = |event: &str| {
        hooks[event].as_array().is_some_and(|groups| {
            groups.iter().any(|group| {
                group["hooks"].as_array().is_some_and(|handlers| {
                    handlers.iter().any(|handler| {
                        handler["command"]
                            .as_str()
                            .is_some_and(|cmd| cmd == script || cmd == quoted)
                    })
                })
            })
        })
    };
    ["PostToolUse", "UserPromptSubmit", "Stop", "SessionEnd"]
        .iter()
        .any(|event| registered(event))
        && (!registered("Stop") || !registered("SessionEnd"))
}

/// Also reaches Homebrew/Cargo upgrades on the next interactive CLI invocation.
/// No marker file, hook changes, or trust grants: disappears once hooks are refreshed.
pub fn print_if_needed() {
    if !std::io::stderr().is_terminal() || !std::io::stdout().is_terminal() {
        return;
    }
    let Some(home) = std::env::var_os("HOME").map(PathBuf::from) else {
        return;
    };
    let codex_home = std::env::var_os("CODEX_HOME")
        .filter(|p| !p.is_empty())
        .map_or_else(|| home.join(".codex"), PathBuf::from);
    let Ok(contents) = std::fs::read_to_string(codex_home.join("hooks.json")) else {
        return;
    };
    let Ok(config) = serde_json::from_str::<Value>(&contents) else {
        return;
    };
    let script = home.join(".config/suvadu/hooks/codex.sh");
    if needs_refresh(&config["hooks"], &script.to_string_lossy()) {
        eprintln!("Suvadu: your Codex hooks need refreshing for session capture.\n  {CODEX_SETUP}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn older_suvadu_hooks_need_session_registration_even_with_other_stop_hooks() {
        let script = "/tmp/suvadu/hooks/codex.sh";
        let hooks = json!({"PostToolUse":[{"hooks":[{"command":script}]}],"Stop":[{"hooks":[{"command":"/team/stop.sh"}]}]});
        assert!(needs_refresh(&hooks, script));
    }
    #[test]
    fn current_or_uninstalled_integrations_need_no_refresh_notice() {
        let script = "/tmp/suvadu/hooks/codex.sh";
        let mut hooks = json!({});
        assert!(!needs_refresh(&hooks, script));
        for event in ["PostToolUse", "UserPromptSubmit", "Stop", "SessionEnd"] {
            hooks[event] = json!([{"hooks":[{"command":format!("'{script}'")}]}]);
        }
        assert!(!needs_refresh(&hooks, script));
        hooks["SessionEnd"] = json!([]);
        assert!(needs_refresh(&hooks, script));
    }
}
