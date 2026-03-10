use super::atomic_write;

// ── Shell RC cleanup ────────────────────────────────────────

/// Remove the Suvadu initialization line from a shell RC file at the given path.
fn cleanup_shell_rc_at(rc_path: &std::path::Path, shell: &str) -> Result<(), std::io::Error> {
    if !rc_path.exists() {
        return Ok(());
    }

    let content = std::fs::read_to_string(rc_path)?;
    let target_line = format!("eval \"$(suv init {shell})\"");

    if !content.contains(&target_line) {
        return Ok(());
    }

    let filtered_content: Vec<String> = content
        .lines()
        .filter(|line| line.trim() != target_line)
        .map(String::from)
        .collect();

    let new_content = filtered_content.join("\n") + "\n";
    atomic_write(rc_path, &new_content)?;

    Ok(())
}

/// Resolve `~/.<filename>` and clean it.
fn cleanup_shell_rc(filename: &str, shell: &str) -> Result<(), std::io::Error> {
    let home = std::env::var("HOME").map_err(|_| {
        std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "HOME environment variable not set",
        )
    })?;
    let rc_path = std::path::PathBuf::from(home).join(filename);
    cleanup_shell_rc_at(&rc_path, shell)
}

/// Remove the Suvadu initialization line from .zshrc if it exists.
pub fn cleanup_zshrc() -> Result<(), std::io::Error> {
    cleanup_shell_rc(".zshrc", "zsh")
}

/// Remove the Suvadu initialization line from .bashrc if it exists.
pub fn cleanup_bashrc() -> Result<(), std::io::Error> {
    cleanup_shell_rc(".bashrc", "bash")
}

/// Remove the Suvadu hook entry from ~/.claude/settings.json.
/// Returns Ok(true) if a hook was removed, Ok(false) if none found or file doesn't exist.
pub fn cleanup_claude_settings() -> Result<bool, Box<dyn std::error::Error>> {
    let home = std::env::var("HOME")?;
    let settings_path = std::path::PathBuf::from(&home)
        .join(".claude")
        .join("settings.json");

    cleanup_claude_settings_at(&settings_path)
}

/// Remove the Suvadu hook from a specific Claude settings file path.
pub fn cleanup_claude_settings_at(
    settings_path: &std::path::Path,
) -> Result<bool, Box<dyn std::error::Error>> {
    if !settings_path.exists() {
        return Ok(false);
    }

    let content = std::fs::read_to_string(settings_path)?;
    let mut settings: serde_json::Value = serde_json::from_str(&content)?;

    let mut removed = false;

    // Remove suvadu entries from both PostToolUse and UserPromptSubmit
    for key in ["PostToolUse", "UserPromptSubmit"] {
        let Some(arr) = settings
            .get_mut("hooks")
            .and_then(|h| h.get_mut(key))
            .and_then(serde_json::Value::as_array_mut)
        else {
            continue;
        };

        let original_len = arr.len();

        arr.retain(|group| {
            group
                .get("hooks")
                .and_then(serde_json::Value::as_array)
                .is_none_or(|hooks| {
                    !hooks.iter().any(|h| {
                        h.get("command")
                            .and_then(serde_json::Value::as_str)
                            .is_some_and(|cmd| cmd.contains("suvadu"))
                    })
                })
        });

        if arr.len() != original_len {
            removed = true;
        }
    }

    if !removed {
        return Ok(false);
    }

    // Clean up empty containers
    if let Some(hooks) = settings.get_mut("hooks").and_then(|h| h.as_object_mut()) {
        hooks.retain(|_, v| !v.as_array().is_some_and(std::vec::Vec::is_empty));

        if hooks.is_empty() {
            if let Some(root) = settings.as_object_mut() {
                root.remove("hooks");
            }
        }
    }

    let updated = serde_json::to_string_pretty(&settings)?;
    atomic_write(settings_path, &updated)?;

    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_cleanup_claude_settings_removes_suvadu_hook() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("settings.json");

        let settings = serde_json::json!({
            "hooks": {
                "PostToolUse": [{
                    "matcher": "Bash",
                    "hooks": [{
                        "type": "command",
                        "command": "/home/user/.config/suvadu/hooks/claude-code-post-tool.sh"
                    }]
                }]
            }
        });
        std::fs::write(&path, serde_json::to_string_pretty(&settings).unwrap()).unwrap();

        let result = cleanup_claude_settings_at(&path).unwrap();
        assert!(result);

        // Verify the hook was removed and empty containers cleaned up
        let content = std::fs::read_to_string(&path).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&content).unwrap();
        assert!(parsed.get("hooks").is_none());
    }

    #[test]
    fn test_cleanup_claude_settings_preserves_other_hooks() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("settings.json");

        let settings = serde_json::json!({
            "hooks": {
                "PostToolUse": [
                    {
                        "matcher": "Bash",
                        "hooks": [{
                            "type": "command",
                            "command": "/home/user/.config/suvadu/hooks/claude-code-post-tool.sh"
                        }]
                    },
                    {
                        "matcher": "Bash",
                        "hooks": [{
                            "type": "command",
                            "command": "/usr/local/bin/other-hook.sh"
                        }]
                    }
                ]
            }
        });
        std::fs::write(&path, serde_json::to_string_pretty(&settings).unwrap()).unwrap();

        let result = cleanup_claude_settings_at(&path).unwrap();
        assert!(result);

        // Verify only Suvadu hook was removed, other hook preserved
        let content = std::fs::read_to_string(&path).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&content).unwrap();
        let post_tool_use = parsed["hooks"]["PostToolUse"].as_array().unwrap();
        assert_eq!(post_tool_use.len(), 1);
        assert!(post_tool_use[0]["hooks"][0]["command"]
            .as_str()
            .unwrap()
            .contains("other-hook"));
    }

    #[test]
    fn test_cleanup_claude_settings_removes_both_hook_types() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("settings.json");

        let settings = serde_json::json!({
            "hooks": {
                "PostToolUse": [{
                    "matcher": "Bash",
                    "hooks": [{
                        "type": "command",
                        "command": "/home/user/.config/suvadu/hooks/claude-code-post-tool.sh"
                    }]
                }],
                "UserPromptSubmit": [{
                    "hooks": [{
                        "type": "command",
                        "command": "/home/user/.config/suvadu/hooks/claude-code-prompt.sh"
                    }]
                }]
            }
        });
        std::fs::write(&path, serde_json::to_string_pretty(&settings).unwrap()).unwrap();

        let result = cleanup_claude_settings_at(&path).unwrap();
        assert!(result);

        let content = std::fs::read_to_string(&path).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&content).unwrap();
        // Both hook types removed, hooks object cleaned up
        assert!(parsed.get("hooks").is_none());
    }

    #[test]
    fn test_cleanup_claude_settings_no_file() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("nonexistent.json");

        let result = cleanup_claude_settings_at(&path).unwrap();
        assert!(!result);
    }

    #[test]
    fn test_cleanup_shell_rc_removes_suvadu_line() {
        let dir = tempfile::TempDir::new().unwrap();
        let rc = dir.path().join(".zshrc");
        std::fs::write(
            &rc,
            "# My config\nexport FOO=bar\neval \"$(suv init zsh)\"\nalias ll='ls -la'\n",
        )
        .unwrap();

        cleanup_shell_rc_at(&rc, "zsh").unwrap();

        let content = std::fs::read_to_string(&rc).unwrap();
        assert!(!content.contains("suv init zsh"));
        assert!(content.contains("export FOO=bar"));
        assert!(content.contains("alias ll='ls -la'"));
    }

    #[test]
    fn test_cleanup_shell_rc_no_suvadu_line() {
        let dir = tempfile::TempDir::new().unwrap();
        let rc = dir.path().join(".bashrc");
        let original = "# My config\nexport FOO=bar\nalias ll='ls -la'\n";
        std::fs::write(&rc, original).unwrap();

        cleanup_shell_rc_at(&rc, "bash").unwrap();

        let content = std::fs::read_to_string(&rc).unwrap();
        assert_eq!(content, original);
    }

    #[test]
    fn test_cleanup_shell_rc_file_missing() {
        let dir = tempfile::TempDir::new().unwrap();
        let rc = dir.path().join(".zshrc");

        // Should be a no-op, not an error
        cleanup_shell_rc_at(&rc, "zsh").unwrap();
        assert!(!rc.exists());
    }

    #[test]
    fn test_cleanup_shell_rc_only_matches_exact_shell() {
        let dir = tempfile::TempDir::new().unwrap();
        let rc = dir.path().join(".zshrc");
        std::fs::write(&rc, "eval \"$(suv init zsh)\"\neval \"$(suv init bash)\"\n").unwrap();

        // Cleaning zsh should only remove the zsh line
        cleanup_shell_rc_at(&rc, "zsh").unwrap();

        let content = std::fs::read_to_string(&rc).unwrap();
        assert!(!content.contains("suv init zsh"));
        assert!(content.contains("suv init bash"));
    }

    #[test]
    fn test_cleanup_claude_settings_no_hooks_key() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("settings.json");
        std::fs::write(&path, r#"{"theme": "dark"}"#).unwrap();

        let result = cleanup_claude_settings_at(&path).unwrap();
        assert!(!result);
    }
}
