//! Cursor session capture, end to end: synthetic hook payloads, shaped like
//! the ones Cursor documents for `beforeSubmitPrompt`, `afterShellExecution`,
//! `afterAgentResponse` and `stop`, piped through the real `suv` binary.
//!
//! Every test runs against a private HOME; nothing here reads or writes the
//! user's own database, config or Cursor data.
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

use serde_json::{json, Value};

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
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env_remove("SUVADU_PAUSED")
            .env_remove("SUVADU_SESSION_ID")
            .env("NO_COLOR", "1");
        cmd
    }

    fn write_config(&self, contents: &str) {
        let path = if cfg!(target_os = "macos") {
            self.home
                .path()
                .join("Library/Application Support/tech.appachi.suvadu/config.toml")
        } else {
            self.home.path().join("config/suvadu/config.toml")
        };
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, contents).unwrap();
    }

    fn project(&self) -> PathBuf {
        let dir = self.home.path().join("project");
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn pipe(cmd: &mut Command, input: &[u8]) -> Output {
        let mut child = cmd
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        // A hook may stop reading early; a broken pipe is its business.
        let _ = child.stdin.take().unwrap().write_all(input);
        child.wait_with_output().unwrap()
    }

    /// Run `hook` with `input` on stdin, as Cursor runs a hook script.
    fn hook_raw(&self, hook: &str, input: &[u8]) -> Output {
        Self::pipe(self.command().arg(hook), input)
    }

    fn hook(&self, hook: &str, event: &Value) -> Output {
        let output = self.hook_raw(hook, event.to_string().as_bytes());
        assert!(
            output.status.success(),
            "`suv {hook}` failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        output
    }

    fn run(&self, args: &[&str]) -> Output {
        let output = self.command().args(args).output().unwrap();
        assert!(
            output.status.success(),
            "`suv {}` failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr)
        );
        output
    }

    fn json(&self, args: &[&str]) -> Value {
        serde_json::from_slice(&self.run(args).stdout).unwrap()
    }

    fn session(&self, id: &str) -> Value {
        self.json(&["agent", "session", id, "--limit", "100"])
    }

    fn sessions(&self) -> Vec<Value> {
        self.json(&["agent", "sessions"])["sessions"]
            .as_array()
            .unwrap()
            .clone()
    }

    fn history(&self) -> Vec<Value> {
        String::from_utf8(self.run(&["history", "--json", "-n", "100"]).stdout)
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect()
    }

    /// The text of every file under the sandbox's data directory.
    fn stored_text(&self) -> String {
        fn walk(path: &Path, out: &mut String) {
            for entry in std::fs::read_dir(path).into_iter().flatten().flatten() {
                let path = entry.path();
                if path.is_dir() {
                    walk(&path, out);
                } else {
                    out.push_str(&String::from_utf8_lossy(
                        &std::fs::read(&path).unwrap_or_default(),
                    ));
                }
            }
        }
        let mut out = String::new();
        let data = if cfg!(target_os = "macos") {
            self.home
                .path()
                .join("Library/Application Support/tech.appachi.suvadu")
        } else {
            self.home.path().join("data/suvadu")
        };
        // Make the WAL's contents part of the main file before reading.
        let _ = self.command().args(["doctor"]).output();
        walk(&data, &mut out);
        out
    }
}

/// The fields Cursor sends with every agent hook.
fn common(event: &str, workspace: &Path, generation: &str) -> Value {
    json!({
        "hook_event_name": event,
        "conversation_id": "conv-1",
        "generation_id": generation,
        "model": "claude-sonnet-5",
        "cursor_version": "2.0.0",
        "workspace_roots": [workspace],
        "user_email": null,
        "transcript_path": null
    })
}

fn with(mut base: Value, extra: &Value) -> Value {
    for (key, value) in extra.as_object().unwrap() {
        base[key] = value.clone();
    }
    base
}

fn prompt(workspace: &Path, generation: &str, text: &str) -> Value {
    with(
        common("beforeSubmitPrompt", workspace, generation),
        &json!({"prompt": text, "attachments": []}),
    )
}

fn shell(workspace: &Path, generation: &str, command: &str) -> Value {
    with(
        common("afterShellExecution", workspace, generation),
        &json!({"command": command, "output": "ok\n", "duration": 120, "sandbox": false}),
    )
}

fn response(workspace: &Path, generation: &str, text: &str) -> Value {
    with(
        common("afterAgentResponse", workspace, generation),
        &json!({"text": text}),
    )
}

fn stop(workspace: &Path, generation: &str, status: &str) -> Value {
    with(
        common("stop", workspace, generation),
        &json!({"status": status, "loop_count": 0}),
    )
}

/// One full turn, as Cursor delivers it.
fn turn(s: &Sandbox, workspace: &Path, generation: &str, ask: &str, answer: &str, cmd: &str) {
    let out = s.hook("hook-cursor-prompt", &prompt(workspace, generation, ask));
    assert_eq!(
        String::from_utf8_lossy(&out.stdout).trim(),
        r#"{"continue":true}"#
    );
    s.hook("hook-cursor", &shell(workspace, generation, cmd));
    s.hook(
        "hook-cursor-session",
        &response(workspace, generation, answer),
    );
    s.hook(
        "hook-cursor-session",
        &stop(workspace, generation, "completed"),
    );
}

fn kinds(session: &Value) -> Vec<String> {
    session["events"]
        .as_array()
        .unwrap()
        .iter()
        .map(|event| event["kind"].as_str().unwrap().to_owned())
        .collect()
}

#[test]
fn a_cursor_conversation_is_captured_with_its_responses_and_linked_commands() {
    let s = Sandbox::new();
    let project = s.project();
    turn(
        &s,
        &project,
        "gen-1",
        "Fix the failing test",
        "Fixed it.",
        "cargo test",
    );
    turn(
        &s,
        &project,
        "gen-2",
        "Now add docs",
        "Added docs.",
        "cargo doc",
    );

    let session = s.session("cursor-conv-1");
    let header = &session["session"];
    assert_eq!(header["agent"], "cursor");
    assert_eq!(header["native_id"], "conv-1");
    assert_eq!(header["cwd"], project.to_string_lossy().as_ref());
    assert_eq!(header["model"], "claude-sonnet-5");
    assert_eq!(header["preview"], "Fix the failing test");
    assert_eq!(
        kinds(&session),
        [
            "prompt",
            "response",
            "turn_completed",
            "prompt",
            "response",
            "turn_completed"
        ]
    );
    assert_eq!(session["events"][1]["data"]["text"], "Fixed it.");
    assert_eq!(session["events"][1]["turn_id"], "gen-1");
    // No token counts are reported by Cursor's hooks: unavailable, not zero.
    assert!(header["usage"].is_null());
    assert_eq!(header["usage_complete"], false);
    assert!(header["capture"]["known_missing"]
        .to_string()
        .contains("Cursor does not report token usage"));

    // Each command belongs to the turn it ran in.
    let commands = session["commands"].as_array().unwrap();
    assert_eq!(commands.len(), 2);
    let turn_of = |command: &str| {
        commands.iter().find(|c| c["command"] == command).unwrap()["turn_id"].clone()
    };
    assert_eq!(turn_of("cargo test"), "gen-1");
    assert_eq!(turn_of("cargo doc"), "gen-2");

    let listed = s.sessions();
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0]["agent"], "cursor");
}

#[test]
fn a_hook_delivered_twice_stores_its_event_once() {
    let s = Sandbox::new();
    let project = s.project();
    for _ in 0..2 {
        s.hook("hook-cursor-prompt", &prompt(&project, "gen-1", "Fix it"));
        s.hook("hook-cursor-session", &response(&project, "gen-1", "Done."));
        s.hook("hook-cursor-session", &stop(&project, "gen-1", "completed"));
    }
    assert_eq!(
        kinds(&s.session("cursor-conv-1")),
        ["prompt", "response", "turn_completed"]
    );
}

#[test]
fn an_aborted_turn_is_recorded_as_interrupted() {
    let s = Sandbox::new();
    let project = s.project();
    s.hook("hook-cursor-prompt", &prompt(&project, "gen-1", "Refactor"));
    s.hook("hook-cursor-session", &stop(&project, "gen-1", "aborted"));
    let session = s.session("cursor-conv-1");
    assert_eq!(kinds(&session), ["prompt", "turn_interrupted"]);
    assert_eq!(session["events"][1]["data"]["status"], "aborted");
}

#[test]
fn a_secret_in_a_cursor_prompt_is_redacted_everywhere_it_is_kept() {
    let s = Sandbox::new();
    let project = s.project();
    let secret = "ghp_abcdefghijklmnopqrstuvwxyz0123";
    let ask = format!("Push with GITHUB_TOKEN={secret} please");
    s.hook("hook-cursor-prompt", &prompt(&project, "gen-1", &ask));
    s.hook("hook-cursor", &shell(&project, "gen-1", "git push"));
    s.hook(
        "hook-cursor-session",
        &response(&project, "gen-1", &format!("Used {secret}")),
    );

    let session = s.session("cursor-conv-1");
    assert!(!session.to_string().contains(secret), "{session}");
    assert!(session["events"][0]["data"]["text"]
        .as_str()
        .unwrap()
        .contains("REDACTED"));
    let entry = s
        .history()
        .into_iter()
        .find(|e| e["command"] == "git push")
        .unwrap();
    let context_prompt = entry["context"]["agent_prompt"].as_str().unwrap();
    assert!(context_prompt.contains("REDACTED"), "{context_prompt}");
    // Nor in the prompt cache file or anywhere else on disk.
    let stored = s.stored_text();
    assert!(stored.contains("Push with GITHUB_TOKEN="));
    assert!(!stored.contains(secret));
}

#[test]
fn project_exclusions_and_disabled_projects_keep_cursor_text_out() {
    let s = Sandbox::new();
    let project = s.project();
    std::fs::write(
        project.join(".suvadu.toml"),
        "exclusions = [\"internal-codename\"]\n",
    )
    .unwrap();
    s.hook("hook-cursor-prompt", &prompt(&project, "gen-1", "Ship it"));
    s.hook(
        "hook-cursor-session",
        &response(&project, "gen-1", "Shipped internal-codename to prod"),
    );
    let session = s.session("cursor-conv-1");
    assert_eq!(kinds(&session), ["prompt"]);
    let stored = s.stored_text();
    assert!(stored.contains("Ship it"));
    assert!(!stored.contains("internal-codename"));

    let off = s.home.path().join("off");
    std::fs::create_dir_all(&off).unwrap();
    std::fs::write(off.join(".suvadu.toml"), "enabled = false\n").unwrap();
    let mut other = prompt(&off, "gen-9", "private work");
    other["conversation_id"] = json!("conv-off");
    s.hook("hook-cursor-prompt", &other);
    assert!(s
        .sessions()
        .iter()
        .all(|session| session["id"] != "cursor-conv-off"));
    assert!(!s.stored_text().contains("private work"));
}

#[test]
fn mcp_excluded_directories_hide_a_cursor_session() {
    let s = Sandbox::new();
    let project = s.project();
    s.write_config(&format!(
        "[mcp]\nexclude_dirs = [\"{}\"]\n",
        project.to_string_lossy()
    ));
    turn(&s, &project, "gen-1", "Fix it", "Done.", "make");
    // The CLI is the owner's own view; MCP is what agents see.
    assert_eq!(s.sessions().len(), 1);
    let request = [
        json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2024-11-05","capabilities":{},"clientInfo":{"name":"t","version":"1"}}}),
        json!({"jsonrpc":"2.0","method":"notifications/initialized"}),
        json!({"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"get_agent_session","arguments":{"session_id":"cursor-conv-1"}}}),
        json!({"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"list_agent_sessions","arguments":{}}}),
    ]
    .iter()
    .map(|message| message.to_string() + "\n")
    .collect::<String>();
    let output = Sandbox::pipe(s.command().arg("mcp-serve"), request.as_bytes());
    let replies = String::from_utf8_lossy(&output.stdout);
    assert!(!replies.contains("Fix it"), "{replies}");
    assert!(!replies.contains("Done."), "{replies}");
    assert!(replies.contains("not found or excluded"), "{replies}");
}

#[test]
fn paused_capture_stores_no_cursor_text_but_lets_the_prompt_through() {
    let s = Sandbox::new();
    let project = s.project();
    let mut cmd = s.command();
    cmd.env("SUVADU_PAUSED", "1").arg("hook-cursor-prompt");
    let out = Sandbox::pipe(
        &mut cmd,
        prompt(&project, "gen-1", "paused prompt")
            .to_string()
            .as_bytes(),
    );
    assert!(out.status.success());
    assert_eq!(
        String::from_utf8_lossy(&out.stdout).trim(),
        r#"{"continue":true}"#
    );
    let mut cmd = s.command();
    cmd.env("SUVADU_PAUSED", "1").arg("hook-cursor-session");
    let out = Sandbox::pipe(
        &mut cmd,
        response(&project, "gen-1", "paused answer")
            .to_string()
            .as_bytes(),
    );
    assert!(out.status.success());
    assert!(s.sessions().is_empty());
    let stored = s.stored_text();
    assert!(!stored.contains("paused prompt"));
    assert!(!stored.contains("paused answer"));
}

#[test]
fn missing_or_malformed_fields_never_block_cursor_and_store_nothing() {
    let s = Sandbox::new();
    let project = s.project();
    let continue_line = r#"{"continue":true}"#;
    for input in [
        b"not json".to_vec(),
        Vec::new(),
        json!({"prompt": "no conversation"})
            .to_string()
            .into_bytes(),
        json!({"conversation_id": "../bad", "prompt": "unsafe id"})
            .to_string()
            .into_bytes(),
    ] {
        let out = s.hook_raw("hook-cursor-prompt", &input);
        assert_eq!(
            String::from_utf8_lossy(&out.stdout).trim(),
            continue_line,
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
    // A response without a workspace, an event name or text is dropped.
    let mut no_workspace = response(&project, "gen-1", "where?");
    no_workspace
        .as_object_mut()
        .unwrap()
        .remove("workspace_roots");
    s.hook("hook-cursor-session", &no_workspace);
    let mut no_name = response(&project, "gen-1", "what?");
    no_name.as_object_mut().unwrap().remove("hook_event_name");
    s.hook("hook-cursor-session", &no_name);
    let mut no_text = response(&project, "gen-1", "");
    no_text.as_object_mut().unwrap().remove("text");
    s.hook("hook-cursor-session", &no_text);
    s.hook("hook-cursor-session", &json!({}));
    assert!(s.sessions().is_empty());

    // Without a generation id the event is still kept, just not in a turn.
    let mut no_generation = response(&project, "gen-1", "unattributed");
    no_generation
        .as_object_mut()
        .unwrap()
        .remove("generation_id");
    s.hook("hook-cursor-session", &no_generation);
    let session = s.session("cursor-conv-1");
    assert_eq!(kinds(&session), ["response"]);
    assert!(session["events"][0]["turn_id"].is_null());
}

#[test]
fn a_megabyte_response_is_kept_cut_to_the_prompt_capture_limit() {
    let s = Sandbox::new();
    let project = s.project();
    let long = format!("start {}", "word ".repeat(400_000));
    assert!(long.len() > 1024 * 1024);
    s.hook("hook-cursor-session", &response(&project, "gen-1", &long));
    let session = s.session("cursor-conv-1");
    let text = session["events"][0]["data"]["text"].as_str().unwrap();
    assert!(text.starts_with("start "));
    assert!(text.chars().count() <= 4000);
    assert_eq!(session["events"][0]["data"]["truncated"], true);

    // Past the 16 MiB hook cap the text cannot be read, and that is said.
    let huge = response(&project, "gen-2", &"x".repeat(17 * 1024 * 1024));
    let out = s.hook_raw("hook-cursor-session", huge.to_string().as_bytes());
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("16 MB"));
}

#[test]
fn deleting_a_cursor_session_removes_its_events_and_commands() {
    let s = Sandbox::new();
    let project = s.project();
    turn(&s, &project, "gen-1", "Fix it", "Done.", "make");
    s.run(&["agent", "delete-session", "cursor-conv-1"]);
    assert!(s.sessions().is_empty());
    assert!(s.history().iter().all(|e| e["command"] != "make"));
    let missing = s
        .command()
        .args(["agent", "session", "cursor-conv-1"])
        .output()
        .unwrap();
    assert!(!missing.status.success());
}

fn hooks_json(s: &Sandbox) -> PathBuf {
    s.home.path().join(".cursor/hooks.json")
}

fn suvadu_commands(parsed: &Value, event: &str) -> Vec<String> {
    parsed["hooks"][event]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|hook| hook["command"].as_str())
        .filter(|command| command.contains("/suvadu/"))
        .map(str::to_owned)
        .collect()
}

#[test]
fn init_cursor_adds_session_hooks_beside_custom_ones_and_is_idempotent() {
    let s = Sandbox::new();
    std::fs::create_dir_all(hooks_json(&s).parent().unwrap()).unwrap();
    let custom = json!({"version": 1, "hooks": {
        "stop": [{"command": "/opt/team/notify.sh", "loop_limit": 2}],
        "afterAgentResponse": [{"command": "./audit.sh"}]
    }});
    std::fs::write(hooks_json(&s), custom.to_string()).unwrap();

    let output = s.run(&["init", "cursor"]);
    let message = String::from_utf8_lossy(&output.stdout);
    assert!(message.contains("cursor-session.sh"), "{message}");
    let first = std::fs::read_to_string(hooks_json(&s)).unwrap();
    let parsed: Value = serde_json::from_str(&first).unwrap();
    assert_eq!(parsed["hooks"]["stop"][0], custom["hooks"]["stop"][0]);
    assert_eq!(
        parsed["hooks"]["afterAgentResponse"][0],
        custom["hooks"]["afterAgentResponse"][0]
    );
    for (event, script) in [
        ("beforeSubmitPrompt", "cursor-prompt.sh"),
        ("afterShellExecution", "cursor-after-shell.sh"),
        ("afterAgentResponse", "cursor-session.sh"),
        ("stop", "cursor-session.sh"),
    ] {
        let commands = suvadu_commands(&parsed, event);
        assert_eq!(commands.len(), 1, "{event}: {commands:?}");
        assert!(commands[0].ends_with(script), "{event}: {commands:?}");
        assert!(Path::new(&commands[0]).is_file());
    }

    s.run(&["init", "cursor"]);
    assert_eq!(first, std::fs::read_to_string(hooks_json(&s)).unwrap());
}

#[test]
fn a_cursor_session_takes_git_snapshots_at_its_first_prompt_and_turn_end() {
    if Command::new("git").arg("--version").output().is_err() {
        eprintln!("skipping: git is not installed");
        return;
    }
    let s = Sandbox::new();
    let project = s.project();
    let git = |args: &[&str]| {
        let status = Command::new("git")
            .args(["-c", "user.name=T", "-c", "user.email=t@example.invalid"])
            .args([
                "-c",
                "commit.gpgsign=false",
                "-c",
                "init.defaultBranch=main",
            ])
            .arg("-C")
            .arg(&project)
            .args(args)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .unwrap();
        assert!(status.success(), "git {args:?}");
    };
    git(&["init", "-q"]);
    std::fs::write(project.join("a.txt"), "a\n").unwrap();
    git(&["add", "."]);
    git(&["commit", "-q", "-m", "first"]);

    s.hook("hook-cursor-prompt", &prompt(&project, "gen-1", "Edit a"));
    std::fs::write(project.join("a.txt"), "edited\n").unwrap();
    s.hook("hook-cursor-session", &stop(&project, "gen-1", "completed"));

    let db = if cfg!(target_os = "macos") {
        s.home
            .path()
            .join("Library/Application Support/tech.appachi.suvadu/history.db")
    } else {
        s.home.path().join("data/suvadu/history.db")
    };
    let repo = suvadu::repository::Repository::new(suvadu::db::init_db(&db).unwrap());
    let (start, end) = repo.git_snapshots("cursor-conv-1").unwrap();
    assert!(start.unwrap().paths.is_empty());
    let end = end.unwrap();
    assert!(end.paths.iter().any(|changed| changed.path == "a.txt"));
}
