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
        if value["hook_event_name"] == "Stop" {
            assert_eq!(result.stdout, b"{}\n");
        } else {
            assert!(
                result.stdout.is_empty(),
                "Logging must not inject prompt context"
            );
        }
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

fn run_hook(s: &Sandbox, command: &str, event: &serde_json::Value) -> Output {
    let mut child = s
        .command()
        .arg(command)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(event.to_string().as_bytes())
        .unwrap();
    child.wait_with_output().unwrap()
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
    assert_eq!(parsed["hooks"]["Stop"][0], config["hooks"]["Stop"][0]);
    assert_eq!(parsed["hooks"]["Stop"].as_array().unwrap().len(), 2);
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
    // Doctor reports integrations per agent, so the broken Claude Code hook
    // shows on that agent's row and its repair in the Repairs section.
    let line = out.lines().find(|l| l.contains("Claude Code")).unwrap();
    assert!(!line.contains('✓'), "Broken hook reported healthy: {line}");
    assert!(
        line.contains("broken"),
        "Broken hook not called out: {line}"
    );
    assert!(
        out.contains("suv init claude-code"),
        "Missing actionable repair:\n{out}"
    );
}

#[test]
fn codex_session_hook_and_cli_correlate_native_events_usage_and_commands() {
    use serde_json::json;
    let s = Sandbox::new();
    let transcript = s.home.path().join("synthetic.jsonl");
    let rows = [
        json!({"type":"session_meta","payload":{"id":"session-123","cwd":s.home.path()}}),
        json!({"type":"turn_context","payload":{"turn_id":"turn-one","cwd":s.home.path(),"model":"fixture-model"}}),
        json!({"type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"Check the synthetic project"}],"internal_chat_message_metadata_passthrough":{"content_item_kinds":["user.text"]}}}),
        json!({"type":"event_msg","payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":70,"output_tokens":30,"total_tokens":100}}}}),
        json!({"type":"response_item","payload":{"type":"message","role":"assistant","phase":"final_answer","content":[{"type":"output_text","text":"The synthetic project is clean."}]}}),
    ];
    let mut file = std::fs::File::create(&transcript).unwrap();
    for mut row in rows {
        row["timestamp"] = json!("2026-09-12T12:00:00Z");
        writeln!(file, "{row}").unwrap();
    }
    s.event(&prompt("turn-one", "Check the synthetic project"));
    s.event(&tool("turn-one", "git status", &json!({"exit_code":0})));
    let stop =
        json!({"hook_event_name":"Stop","session_id":"session-123","transcript_path":transcript});
    s.event(&stop);
    s.event(&stop);
    let output = s.run(&["agent", "session", "codex-session-123"]);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let session: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(session["events"].as_array().unwrap().len(), 4);
    assert_eq!(session["session"]["usage"]["total_tokens"], 100);
    assert_eq!(session["session"]["model"], "fixture-model");
    assert_eq!(session["session"]["models"], json!(["fixture-model"]));
    assert_eq!(session["commands"].as_array().unwrap().len(), 1);
    assert_eq!(session["commands"][0]["turn_id"], "turn-one");
    assert_eq!(
        session["events"][1]["data"]["text"],
        "Check the synthetic project"
    );
    let list = s.run(&["sessions", "--list"]);
    assert!(list.status.success());
    let list = String::from_utf8(list.stdout).unwrap();
    assert!(list.contains("AI"));
    assert!(list.contains("fixture-model"));
    assert!(list.contains("100"));
    assert!(s
        .run(&["agent", "delete-session", "codex-session-123"])
        .status
        .success());
    let output = s.run(&["agent", "sessions"]);
    let sessions: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(sessions["sessions"], json!([]));
    assert!(s.history().is_empty());
}

#[test]
#[allow(clippy::too_many_lines)] // End-to-end fixture keeps all privacy and usage cases visible.
fn claude_session_hook_imports_prompts_responses_models_and_usage_once() {
    use serde_json::json;
    let s = Sandbox::new();
    let transcript = s.home.path().join("claude-session.jsonl");
    let usage = json!({
        "input_tokens": 2,
        "cache_creation_input_tokens": 10,
        "cache_read_input_tokens": 20,
        "output_tokens": 5
    });
    let rows = [
        json!({
            "type":"user", "sessionId":"session-123", "uuid":"user-1",
            "promptId":"prompt-1", "timestamp":"2020-01-01T10:00:00Z",
            "cwd":s.home.path(), "promptSource":"typed", "origin":{"kind":"human"},
            "message":{"role":"user","content":"Inspect the project"}
        }),
        json!({
            "type":"user", "sessionId":"session-123", "uuid":"meta-1",
            "promptId":"prompt-1", "timestamp":"2020-01-01T10:00:00Z",
            "cwd":s.home.path(), "isMeta":true,
            "message":{"role":"user","content":"injected metadata secret"}
        }),
        json!({
            "type":"assistant", "sessionId":"session-123", "uuid":"assistant-thinking",
            "requestId":"request-1", "apiBlockIndex":0, "parentUuid":"user-1",
            "timestamp":"2026-09-13T10:00:01Z", "cwd":s.home.path(),
            "message":{"id":"message-1","role":"assistant","model":"claude-test-model",
                "content":[{"type":"thinking","thinking":"private reasoning"}],"usage":usage}
        }),
        json!({
            "type":"assistant", "sessionId":"session-123", "uuid":"assistant-text",
            "requestId":"request-1", "apiBlockIndex":1, "parentUuid":"assistant-thinking",
            "timestamp":"2026-09-13T10:00:02Z", "cwd":s.home.path(),
            "message":{"id":"message-1","role":"assistant","model":"claude-test-model",
                "content":[{"type":"text","text":"The project is clean."}],"usage":usage}
        }),
        json!({
            "type":"assistant", "sessionId":"session-123", "uuid":"assistant-text-replay",
            "requestId":"request-1", "apiBlockIndex":1, "parentUuid":"assistant-thinking",
            "timestamp":"2026-09-13T10:00:02Z", "cwd":s.home.path(),
            "message":{"id":"message-1","role":"assistant","model":"claude-test-model",
                "content":[{"type":"text","text":"The project is clean."}],"usage":usage}
        }),
        json!({
            "type":"assistant", "sessionId":"session-123", "uuid":"assistant-tool",
            "requestId":"request-1", "apiBlockIndex":2, "parentUuid":"assistant-text",
            "timestamp":"2026-09-13T10:00:03Z", "cwd":s.home.path(),
            "message":{"id":"message-1","role":"assistant","model":"claude-test-model",
                "content":[{"type":"tool_use","id":"tool-1","name":"Bash","input":{"command":"secret"}}],"usage":usage}
        }),
        json!({
            "type":"assistant", "sessionId":"session-123", "uuid":"assistant-api-error",
            "requestId":"request-error", "isApiErrorMessage":true,
            "timestamp":"2026-09-13T10:00:04Z", "cwd":s.home.path(),
            "message":{"id":"message-error","role":"assistant","model":"<synthetic>",
                "content":[{"type":"text","text":"The provider returned an error."}],
                "usage":{"input_tokens":0,"output_tokens":0}}
        }),
    ];
    let mut file = std::fs::File::create(&transcript).unwrap();
    for row in rows {
        writeln!(file, "{row}").unwrap();
    }

    let mut child = s
        .command()
        .arg("hook-claude-session")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(
            json!({
                "hook_event_name":"Stop", "session_id":"session-123",
                "transcript_path":transcript, "cwd":s.home.path()
            })
            .to_string()
            .as_bytes(),
        )
        .unwrap();
    let result = child.wait_with_output().unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );

    let output = s.run(&["agent", "session", "claude-session-123"]);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let session: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(session["session"]["agent"], "claude-code");
    assert_eq!(session["session"]["model"], "claude-test-model");
    assert_eq!(session["session"]["models"], json!(["claude-test-model"]));
    assert_eq!(session["session"]["usage"]["input_tokens"], 32);
    assert_eq!(session["session"]["usage"]["cached_input_tokens"], 20);
    assert_eq!(session["session"]["usage"]["cache_write_input_tokens"], 10);
    assert_eq!(session["session"]["usage"]["output_tokens"], 5);
    assert_eq!(session["session"]["usage"]["total_tokens"], 37);
    let events = session["events"].as_array().unwrap();
    assert_eq!(
        events
            .iter()
            .filter(|event| event["kind"] == "usage")
            .count(),
        1
    );
    assert!(events
        .iter()
        .any(|event| event["kind"] == "prompt" && event["data"]["text"] == "Inspect the project"));
    assert!(events.iter().any(
        |event| event["kind"] == "response" && event["data"]["text"] == "The project is clean."
    ));
    assert_eq!(
        events
            .iter()
            .filter(|event| event["kind"] == "response"
                && event["data"]["text"] == "The project is clean.")
            .count(),
        1
    );
    let stored = serde_json::to_string(&session).unwrap();
    assert!(!stored.contains("private reasoning"));
    assert!(!stored.contains("secret"));
    assert!(!stored.contains("injected metadata"));
}

#[test]
fn claude_init_adds_session_hooks_without_replacing_custom_hooks() {
    let s = Sandbox::new();
    let settings = s.home.path().join(".claude/settings.json");
    std::fs::create_dir_all(settings.parent().unwrap()).unwrap();
    let custom = serde_json::json!({"hooks":{
        "Stop":[{"hooks":[{"type":"command","command":"/opt/team/stop.sh"}]}]
    }});
    std::fs::write(&settings, custom.to_string()).unwrap();

    let output = s.run(&["init", "claude-code"]);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let message = String::from_utf8_lossy(&output.stdout);
    assert!(message.contains("Relaunch Claude Code"));
    assert!(message.contains("suv sessions"));
    assert!(message.contains("reported tokens"));
    let parsed: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&settings).unwrap()).unwrap();
    assert!(parsed["hooks"]["Stop"]
        .as_array()
        .unwrap()
        .iter()
        .any(
            |group| group["hooks"].as_array().unwrap().iter().any(|hook| {
                hook["command"]
                    .as_str()
                    .is_some_and(|command| command.ends_with("claude-code-session.sh"))
            })
        ));
    assert_eq!(parsed["hooks"]["Stop"][0], custom["hooks"]["Stop"][0]);
    assert!(parsed["hooks"]["SessionEnd"]
        .as_array()
        .unwrap()
        .iter()
        .any(
            |group| group["hooks"].as_array().unwrap().iter().any(|hook| {
                hook["command"]
                    .as_str()
                    .is_some_and(|command| command.ends_with("claude-code-session.sh"))
            })
        ));
    assert!(s
        .home
        .path()
        .join(".config/suvadu/hooks/claude-code-session.sh")
        .is_file());

    let first = std::fs::read_to_string(&settings).unwrap();
    assert!(s.run(&["init", "claude-code"]).status.success());
    assert_eq!(first, std::fs::read_to_string(settings).unwrap());
}

#[test]
fn import_session_auto_detects_claude_transcripts() {
    let s = Sandbox::new();
    let transcript = s.home.path().join("claude-import.jsonl");
    let row = serde_json::json!({
        "type":"user", "sessionId":"import-123", "uuid":"user-1",
        "promptId":"prompt-1", "timestamp":"2026-09-13T10:00:00Z",
        "cwd":s.home.path(), "promptSource":"typed", "origin":{"kind":"human"},
        "message":{"role":"user","content":"Imported prompt"}
    });
    let ignored = serde_json::json!({
        "type":"queue-operation", "operation":"enqueue", "timestamp":"2026-09-13T09:59:59Z"
    });
    std::fs::write(&transcript, format!("{ignored}\n{row}\n")).unwrap();

    let output = s.run(&["agent", "import-session", transcript.to_str().unwrap()]);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let imported: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(imported["session_id"], "claude-import-123");
}

#[test]
fn claude_commands_link_to_their_own_prompt_ids() {
    let s = Sandbox::new();
    for (prompt, command) in [
        ("First request", "printf first"),
        ("Second request", "printf second"),
    ] {
        let output = run_hook(
            &s,
            "hook-claude-prompt",
            &serde_json::json!({
                "hook_event_name":"UserPromptSubmit", "session_id":"session-123",
                "cwd":s.home.path(), "prompt":prompt
            }),
        );
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let output = run_hook(
            &s,
            "hook-claude-code",
            &serde_json::json!({
                "hook_event_name":"PostToolUse", "session_id":"session-123",
                "cwd":s.home.path(), "tool_name":"Bash",
                "tool_input":{"command":command}, "tool_response":{"exit_code":0}
            }),
        );
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    let entries = s.history();
    let first = entries
        .iter()
        .find(|entry| entry["command"] == "printf first")
        .unwrap();
    assert!(first["context"].get("agent_turn_id").is_none());
    assert_eq!(first["context"]["agent_prompt"], "First request");
    let second = entries
        .iter()
        .find(|entry| entry["command"] == "printf second")
        .unwrap();
    assert!(second["context"].get("agent_turn_id").is_none());
    assert_eq!(second["context"]["agent_prompt"], "Second request");

    let transcript = s.home.path().join("claude-linked.jsonl");
    let rows = [
        serde_json::json!({
            "type":"user", "sessionId":"session-123", "uuid":"user-1",
            "promptId":"prompt-1", "timestamp":"2020-01-01T10:00:00Z",
            "cwd":s.home.path(), "promptSource":"typed", "origin":{"kind":"human"},
            "message":{"role":"user","content":"First request"}
        }),
        serde_json::json!({
            "type":"user", "sessionId":"session-123", "uuid":"user-2",
            "promptId":"prompt-2", "timestamp":"2020-01-01T10:01:00Z",
            "cwd":s.home.path(), "promptSource":"typed", "origin":{"kind":"human"},
            "message":{"role":"user","content":"Second request"}
        }),
    ];
    let mut file = std::fs::File::create(&transcript).unwrap();
    for row in rows {
        writeln!(file, "{row}").unwrap();
    }
    let hook = run_hook(
        &s,
        "hook-claude-session",
        &serde_json::json!({
            "hook_event_name":"Stop", "session_id":"session-123",
            "transcript_path":transcript, "cwd":s.home.path()
        }),
    );
    assert!(
        hook.status.success(),
        "{}",
        String::from_utf8_lossy(&hook.stderr)
    );

    let output = s.run(&["agent", "session", "claude-session-123"]);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let session: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    let commands = session["commands"].as_array().unwrap();
    assert!(commands.iter().any(|command| {
        command["command"] == "printf first" && command["turn_id"] == "prompt-1"
    }));
    assert!(commands.iter().any(|command| {
        command["command"] == "printf second" && command["turn_id"] == "prompt-2"
    }));
}

/// `suv doctor` is a diagnostic: it must report a missing database rather than
/// quietly creating one, so a clean machine still looks clean afterwards.
#[test]
fn doctor_does_not_create_a_database_on_a_clean_machine() {
    fn contains_db(dir: &Path) -> bool {
        std::fs::read_dir(dir).is_ok_and(|entries| {
            entries.flatten().any(|entry| {
                let path = entry.path();
                if path.is_dir() {
                    contains_db(&path)
                } else {
                    path.file_name().is_some_and(|name| name == "history.db")
                }
            })
        })
    }

    let s = Sandbox::new();
    let out = String::from_utf8(s.run(&["doctor"]).stdout).unwrap();
    let line = out
        .lines()
        .find(|l| l.contains("Database"))
        .unwrap_or_else(|| panic!("no Database row:\n{out}"));
    assert!(line.contains("not created yet"), "{line}");
    assert!(
        !contains_db(s.home.path()),
        "doctor must not create a database:\n{out}"
    );
}
