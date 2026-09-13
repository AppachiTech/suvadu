//! Upgrade reminders without modifying agent configuration or protocol output.
use serde_json::Value;
use std::io::IsTerminal;
use std::path::PathBuf;

pub const CODEX_SETUP: &str = "Codex session capture: run `suv init codex` with the updated binary.\n  In the Codex terminal CLI, open /hooks and review/trust Suvadu's Stop and SessionEnd hooks.\n  Then relaunch Codex; for the VS Code extension, fully quit and reopen VS Code.";
pub const CLAUDE_SETUP: &str = "Claude Code session capture: run `suv init claude-code` with the updated binary.\n  Then relaunch Claude Code; for the VS Code extension, fully quit and reopen VS Code.";

pub const AGENT_UPGRADE: &str = "After updating agent integrations:\n  Codex: run `suv init codex`, then open /hooks in the Codex terminal CLI and review/trust Suvadu hooks.\n  Relaunch Codex; for its VS Code extension, fully quit and reopen VS Code.\n  Claude Code: run `suv init claude-code`, then relaunch Claude Code; for its VS Code extension, fully quit and reopen VS Code.\n  Native session, model, and reported-token capture supports Codex and Claude Code.";

fn registered(hooks: &Value, event: &str, script: &str) -> bool {
    let quoted = crate::integrations::shell_escape(script);
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
}

fn needs_refresh(hooks: &Value, script: &str) -> bool {
    ["PostToolUse", "UserPromptSubmit", "Stop", "SessionEnd"]
        .iter()
        .any(|event| registered(hooks, event, script))
        && (!registered(hooks, "Stop", script) || !registered(hooks, "SessionEnd", script))
}

fn needs_claude_refresh(hooks: &Value, post_script: &str, session_script: &str) -> bool {
    registered(hooks, "PostToolUse", post_script)
        && (!registered(hooks, "Stop", session_script)
            || !registered(hooks, "SessionEnd", session_script))
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
    if let Ok(contents) = std::fs::read_to_string(codex_home.join("hooks.json")) {
        if let Ok(config) = serde_json::from_str::<Value>(&contents) {
            let script = home.join(".config/suvadu/hooks/codex.sh");
            if needs_refresh(&config["hooks"], &script.to_string_lossy()) {
                eprintln!("Suvadu: your Codex hooks need refreshing for session capture.\n  {CODEX_SETUP}");
            }
        }
    }
    if let Ok(contents) = std::fs::read_to_string(home.join(".claude/settings.json")) {
        if let Ok(config) = serde_json::from_str::<Value>(&contents) {
            let script = home.join(".config/suvadu/hooks/claude-code-session.sh");
            let installed = home.join(".config/suvadu/hooks/claude-code-post-tool.sh");
            if needs_claude_refresh(
                &config["hooks"],
                &installed.to_string_lossy(),
                &script.to_string_lossy(),
            ) {
                eprintln!("Suvadu: your Claude Code hooks need refreshing for session capture.\n  {CLAUDE_SETUP}");
            }
        }
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

    #[test]
    fn update_message_describes_claude_native_session_capture() {
        assert!(AGENT_UPGRADE.contains("Codex and Claude Code"));
        assert!(AGENT_UPGRADE.contains("suv init claude-code"));
        assert!(!AGENT_UPGRADE.contains("Claude capture is planned"));
    }

    #[test]
    fn current_claude_hooks_do_not_request_another_refresh() {
        let post = "/tmp/suvadu/hooks/claude-code-post-tool.sh";
        let session = "/tmp/suvadu/hooks/claude-code-session.sh";
        let hooks = json!({
            "PostToolUse":[{"hooks":[{"command":post}]}],
            "UserPromptSubmit":[{"hooks":[{"command":"/tmp/suvadu/hooks/claude-code-prompt.sh"}]}],
            "Stop":[{"hooks":[{"command":session}]}],
            "SessionEnd":[{"hooks":[{"command":session}]}]
        });
        assert!(!needs_claude_refresh(&hooks, post, session));
        let stale = json!({
            "PostToolUse":[{"hooks":[{"command":post}]}],
            "UserPromptSubmit":[{"hooks":[{"command":"/tmp/suvadu/hooks/claude-code-prompt.sh"}]}]
        });
        assert!(needs_claude_refresh(&stale, post, session));
    }
}
