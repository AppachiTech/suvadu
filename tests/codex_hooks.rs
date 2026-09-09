//! End-to-end hook tests use a private home and real CLI/database.
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

struct Sandbox {
    home: tempfile::TempDir,
}
impl Sandbox {
    fn new() -> Self {
        Self {
            home: tempfile::tempdir().unwrap(),
        }
    }
    fn command(&self) -> Command {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_suv"));
        cmd.env("HOME", self.home.path())
            .env("XDG_DATA_HOME", self.home.path().join("data"))
            .env("XDG_CONFIG_HOME", self.home.path().join("config"))
            .env("CODEX_HOME", self.home.path().join(".codex"))
            .env_remove("SUVADU_PAUSED")
            .env("NO_COLOR", "1");
        cmd
    }
    fn run(&self, args: &[&str]) -> Output {
        self.command().args(args).output().unwrap()
    }
    fn event(&self, value: &serde_json::Value) {
        let mut child = self
            .command()
            .arg("hook-codex")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        child
            .stdin
            .take()
            .unwrap()
            .write_all(value.to_string().as_bytes())
            .unwrap();
        let result = child.wait_with_output().unwrap();
        assert!(
            result.status.success(),
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
        assert!(
            result.stdout.is_empty(),
            "Logging must not inject prompt context"
        );
    }
    fn history(&self) -> Vec<serde_json::Value> {
        let output = self.run(&["history", "--json", "-n", "100"]);
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8(output.stdout)
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect()
    }
    fn hooks_path(&self) -> PathBuf {
        self.home.path().join(".codex/hooks.json")
    }
}

fn prompt(turn: &str, text: &str) -> serde_json::Value {
    serde_json::json!({"hook_event_name":"UserPromptSubmit", "session_id":"session-123", "turn_id":turn, "cwd":"/project", "prompt":text})
}
fn tool(turn: &str, command: &str, response: &serde_json::Value) -> serde_json::Value {
    serde_json::json!({"hook_event_name":"PostToolUse", "session_id":"session-123", "turn_id":turn, "tool_use_id":format!("call-{turn}"), "tool_name":"Bash", "cwd":"/project", "tool_input":{"command":command}, "tool_response":response})
}

#[test]
fn codex_commands_keep_their_turn_prompt_and_real_executor() {
    let s = Sandbox::new();
    s.event(&prompt("turn-one", "Check the project"));
    s.event(&prompt("turn-two", "Check the failures"));
    s.event(&tool(
        "turn-one",
        "git status",
        &serde_json::json!({"exit_code":0}),
    ));
    s.event(&tool(
        "turn-two",
        "false",
        &serde_json::json!({"exit_code":1}),
    ));
    let entries = s.history();
    assert_eq!(entries.len(), 2);
    let git = entries
        .iter()
        .find(|e| e["command"] == "git status")
        .unwrap();
    assert_eq!(git["executor"], "openai-codex");
    assert_eq!(git["executor_type"], "agent");
    assert_eq!(git["session_id"], "codex-session-123");
    assert_eq!(git["context"]["agent_prompt"], "Check the project");
    assert_eq!(git["exit_code"], 0);
    let failed = entries.iter().find(|e| e["command"] == "false").unwrap();
    assert_eq!(failed["exit_code"], 1);
    assert_eq!(failed["context"]["agent_prompt"], "Check the failures");
}

#[test]
fn codex_does_not_assume_success_or_attach_another_turn_prompt() {
    let s = Sandbox::new();
    s.event(&prompt("old-turn", "Old prompt"));
    s.event(&tool(
        "new-turn",
        "some-command",
        &serde_json::json!("arbitrary output"),
    ));
    let entries = s.history();
    assert_eq!(entries.len(), 1);
    assert!(entries[0]["exit_code"].is_null());
    assert!(entries[0]["context"]["agent_prompt"].is_null());
}

#[test]
fn codex_ignores_non_shell_events_invalid_ids_and_disabled_recording() {
    let s = Sandbox::new();
    let mut event = tool(
        "turn-one",
        "do not record",
        &serde_json::json!({"exit_code":0}),
    );
    event["tool_name"] = "apply_patch".into();
    s.event(&event);
    event["tool_name"] = "Bash".into();
    event["session_id"] = "../../escape".into();
    s.event(&event);
    assert!(s.run(&["disable"]).status.success());
    s.event(&prompt("turn-one", "Do not store this prompt"));
    s.event(&tool(
        "turn-one",
        "echo disabled",
        &serde_json::json!({"exit_code":0}),
    ));
    assert!(s.history().is_empty());
}

#[test]
fn codex_redacts_prompts_before_caching_and_recording() {
    fn check_cache(path: &Path, secret: &str) -> usize {
        let mut count = 0;
        for entry in std::fs::read_dir(path).unwrap().flatten() {
            if entry.file_type().unwrap().is_dir() {
                count += check_cache(&entry.path(), secret);
            } else if entry.path().extension().is_some_and(|ext| ext == "prompt") {
                assert!(!std::fs::read_to_string(entry.path())
                    .unwrap()
                    .contains(secret));
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    assert_eq!(
                        entry.metadata().unwrap().permissions().mode() & 0o777,
                        0o600
                    );
                }
                count += 1;
            }
        }
        count
    }

    let s = Sandbox::new();
    let secret = "sample_password_for_hook_regression";
    s.event(&prompt(
        "turn-secret",
        &format!("Check curl --password={secret}"),
    ));
    s.event(&tool(
        "turn-secret",
        "echo redaction-check",
        &serde_json::json!({"exit_code":0}),
    ));
    let entries = s.history();
    let stored = entries[0]["context"]["agent_prompt"].as_str().unwrap();
    assert!(stored.contains("REDACTED"));
    assert!(!stored.contains(secret));
    assert_eq!(check_cache(s.home.path(), secret), 1);
}

#[test]
fn codex_init_leaves_malformed_configuration_untouched() {
    let s = Sandbox::new();
    std::fs::create_dir_all(s.hooks_path().parent().unwrap()).unwrap();
    let malformed = "{broken json";
    std::fs::write(s.hooks_path(), malformed).unwrap();
    assert!(!s.run(&["init", "codex"]).status.success());
    assert_eq!(std::fs::read_to_string(s.hooks_path()).unwrap(), malformed);
    assert!(!s.home.path().join(".config/suvadu/hooks/codex.sh").exists());
}

#[test]
fn codex_init_migrates_only_suvadu_handlers_and_is_idempotent() {
    let s = Sandbox::new();
    std::fs::create_dir_all(s.hooks_path().parent().unwrap()).unwrap();
    let old_script = s
        .home
        .path()
        .join(".config/suvadu/hooks/claude-code-post-tool.sh");
    let config = serde_json::json!({"description":"Keep this", "hooks":{
      "PostToolUse":[{"matcher":"Bash", "hooks":[
        {"type":"command","command":old_script},
        {"type":"command","command":"/opt/team/audit.sh","timeout":7}]}],
      "Stop":[{"hooks":[{"type":"command","command":"/opt/team/stop.sh"}]}]
    }});
    std::fs::write(s.hooks_path(), config.to_string()).unwrap();
    let result = s.run(&["init", "codex"]);
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let first = std::fs::read_to_string(s.hooks_path()).unwrap();
    let parsed: serde_json::Value = serde_json::from_str(&first).unwrap();
    assert_eq!(parsed["description"], "Keep this");
    assert_eq!(parsed["hooks"]["Stop"], config["hooks"]["Stop"]);
    let handlers: Vec<_> = parsed["hooks"]["PostToolUse"]
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|group| group["hooks"].as_array().unwrap())
        .collect();
    assert!(handlers
        .iter()
        .any(|h| h["command"] == "/opt/team/audit.sh" && h["timeout"] == 7));
    assert!(!handlers
        .iter()
        .any(|h| h["command"] == old_script.to_string_lossy().as_ref()));
    assert_eq!(handlers.len(), 2);
    assert!(s.run(&["init", "codex"]).status.success());
    assert_eq!(first, std::fs::read_to_string(s.hooks_path()).unwrap());
    assert!(Path::new(&s.home.path().join(".config/suvadu/hooks/codex.sh")).is_file());
}

#[cfg(unix)]
#[test]
fn agent_hooks_find_the_replacement_binary_after_install_location_changes() {
    use std::os::unix::fs::PermissionsExt;
    let s = Sandbox::new();
    let original = s.home.path().join("old install/suv");
    std::fs::create_dir_all(original.parent().unwrap()).unwrap();
    std::fs::copy(env!("CARGO_BIN_EXE_suv"), &original).unwrap();
    let result = Command::new(&original)
        .env("HOME", s.home.path())
        .args(["init", "claude-code"])
        .output()
        .unwrap();
    assert!(result.status.success());
    // A replacement executable records exactly what it receives, without a real DB.
    let replacement = s.home.path().join("new install");
    std::fs::create_dir_all(&replacement).unwrap();
    let executable = replacement.join("suv");
    std::fs::write(&executable, "#!/bin/sh\nprintf '%s\\n' \"$1\"\ncat\n").unwrap();
    std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o700)).unwrap();
    std::fs::remove_file(&original).unwrap();
    let mut child = Command::new(
        s.home
            .path()
            .join(".config/suvadu/hooks/claude-code-post-tool.sh"),
    )
    .env("PATH", format!("{}:/usr/bin:/bin", replacement.display()))
    .stdin(Stdio::piped())
    .stdout(Stdio::piped())
    .stderr(Stdio::piped())
    .spawn()
    .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(b"{\"example\":true}")
        .unwrap();
    let result = child.wait_with_output().unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    assert_eq!(
        String::from_utf8(result.stdout).unwrap(),
        "hook-claude-code\n{\"example\":true}"
    );
}

#[test]
fn doctor_reports_a_stale_agent_hook_instead_of_counting_it_as_healthy() {
    let s = Sandbox::new();
    let hooks = s.home.path().join(".config/suvadu/hooks");
    std::fs::create_dir_all(&hooks).unwrap();
    std::fs::write(
        hooks.join("claude-code-post-tool.sh"),
        "#!/bin/bash\nexec '/missing/suv' hook-claude-code 2>/dev/null\n",
    )
    .unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(
            hooks.join("claude-code-post-tool.sh"),
            std::fs::Permissions::from_mode(0o700),
        )
        .unwrap();
    }
    let result = s.run(&["doctor"]);
    let out = String::from_utf8(result.stdout).unwrap();
    let line = out.lines().find(|l| l.contains("Agent hooks")).unwrap();
    assert!(!line.contains('✓'), "Broken hook reported healthy: {line}");
    assert!(
        line.contains("suv init"),
        "Missing actionable repair: {line}"
    );
}
