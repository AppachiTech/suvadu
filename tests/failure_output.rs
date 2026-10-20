//! Failure output, end to end: every path that can see what a failed
//! command printed — the agent hooks, `suv add --output-stdin` (the `OpenCode`
//! plugin) and `suv wrap` — keeps a cleaned, redacted tail only when
//! `[capture] failure_output` is on, and the tail goes wherever its command
//! goes.
//!
//! Every test runs the real binary against a private HOME; nothing here
//! reads or writes the user's own database or config.

use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Output, Stdio};

/// A GitHub token, which the built-in redaction rules recognise.
const TOKEN: &str = "ghp_aBcDeFgHiJkLmNoPqRsTuVwXyZ0123456789";

struct Sandbox {
    home: tempfile::TempDir,
}

impl Sandbox {
    /// A sandbox with failure-output capture on.
    fn capturing() -> Self {
        let s = Self::new();
        s.write_config("[capture]\nfailure_output = true\n");
        s
    }

    /// A sandbox with the default config.
    fn new() -> Self {
        Self {
            home: tempfile::tempdir().unwrap(),
        }
    }

    fn write_config(&self, contents: &str) {
        let path = self.config_path();
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, contents).unwrap();
    }

    fn config_path(&self) -> PathBuf {
        let home = self.home.path();
        if cfg!(target_os = "macos") {
            home.join("Library/Application Support/tech.appachi.suvadu/config.toml")
        } else {
            home.join("config/suvadu/config.toml")
        }
    }

    /// A project directory inside the sandbox.
    fn project(&self) -> PathBuf {
        let dir = self.home.path().join("project");
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn command(&self) -> Command {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_suv"));
        cmd.env("HOME", self.home.path())
            .env("XDG_DATA_HOME", self.home.path().join("data"))
            .env("XDG_CONFIG_HOME", self.home.path().join("config"))
            .env("CODEX_HOME", self.home.path().join(".codex"))
            .env_remove("SUVADU_PAUSED")
            .env_remove("SUVADU_SESSION_ID")
            .env("NO_COLOR", "1");
        cmd
    }

    fn run(&self, args: &[&str]) -> Output {
        self.command().args(args).output().unwrap()
    }

    fn run_with_stdin(&self, args: &[&str], input: &str) -> Output {
        let mut child = self
            .command()
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        child
            .stdin
            .take()
            .unwrap()
            .write_all(input.as_bytes())
            .unwrap();
        let output = child.wait_with_output().unwrap();
        assert!(
            output.status.success(),
            "`suv {}` failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr)
        );
        output
    }

    fn hook(&self, hook: &str, event: &serde_json::Value) {
        self.run_with_stdin(&[hook], &event.to_string());
    }

    /// Every recorded command as exported, oldest first.
    fn exported(&self) -> Vec<serde_json::Value> {
        let output = self.run(&["export", "--format", "jsonl"]);
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

    /// The one exported command `command`.
    fn exported_command(&self, command: &str) -> serde_json::Value {
        let mut matching: Vec<_> = self
            .exported()
            .into_iter()
            .filter(|e| e["command"] == command)
            .collect();
        assert_eq!(matching.len(), 1, "{command} recorded once");
        matching.remove(0)
    }
}

fn claude_failure(cwd: &std::path::Path, command: &str, error: &str) -> serde_json::Value {
    serde_json::json!({
        "hook_event_name":"PostToolUseFailure", "session_id":"session-1",
        "cwd":cwd, "tool_name":"Bash", "tool_input":{"command":command},
        "error":error
    })
}

#[test]
fn nothing_is_kept_by_default() {
    let s = Sandbox::new();
    let cwd = s.project();
    s.hook(
        "hook-claude-code-failure",
        &claude_failure(&cwd, "make test", "Exit code 2\nmake: *** [test] Error 2"),
    );
    let entry = s.exported_command("make test");
    assert_eq!(entry["exit_code"], 2);
    assert!(entry.get("failure_output").is_none(), "{entry}");
}

#[test]
fn claude_code_failures_keep_their_error_text_without_the_exit_line() {
    let s = Sandbox::capturing();
    let cwd = s.project();
    let error = format!(
        "Exit code 1\n\u{1b}[31merror\u{1b}[0m: could not push to \
         https://bot:{TOKEN}@github.com/org/repo.git\nhint: pull first"
    );
    s.hook(
        "hook-claude-code-failure",
        &claude_failure(&cwd, "git push", &error),
    );
    let output = &s.exported_command("git push")["failure_output"];
    assert_eq!(output["stream"], "output");
    let tail = output["tail"].as_str().unwrap();
    assert!(tail.starts_with("error: could not push"), "{tail}");
    assert!(tail.ends_with("hint: pull first"), "{tail}");
    assert!(!tail.contains("ghp_aBcDeF"), "the token was stored: {tail}");
    assert!(!tail.contains('\u{1b}'), "{tail}");
}

#[test]
fn claude_code_successes_keep_nothing_even_with_output() {
    let s = Sandbox::capturing();
    let cwd = s.project();
    s.hook(
        "hook-claude-code",
        &serde_json::json!({
            "hook_event_name":"PostToolUse", "session_id":"session-1", "cwd":cwd,
            "tool_name":"Bash", "tool_input":{"command":"cargo build"},
            "tool_response":{"stdout":"Finished","stderr":"warning: unused","interrupted":false}
        }),
    );
    assert!(s
        .exported_command("cargo build")
        .get("failure_output")
        .is_none());
}

#[test]
fn a_megabyte_error_keeps_only_its_last_lines() {
    let s = Sandbox::capturing();
    let cwd = s.project();
    let error = format!(
        "Exit code 101\n{}error: test failed, to rerun pass `--lib`",
        "test case ... ok\n".repeat(200_000)
    );
    s.hook(
        "hook-claude-code-failure",
        &claude_failure(&cwd, "cargo test", &error),
    );
    let output = &s.exported_command("cargo test")["failure_output"];
    let tail = output["tail"].as_str().unwrap();
    assert!(tail.len() <= 4096, "{}", tail.len());
    assert!(tail.ends_with("error: test failed, to rerun pass `--lib`"));
    assert!(
        tail.starts_with("test case"),
        "line-aligned: {:?}",
        &tail[..20]
    );
    assert!(output["truncated_bytes"].as_i64().unwrap() > 3_000_000);
}

#[test]
fn cursor_failures_keep_their_output() {
    let s = Sandbox::capturing();
    let cwd = s.project();
    s.hook(
        "hook-cursor",
        &serde_json::json!({
            "command":"npm test", "cwd":cwd, "exit_code":1,
            "conversation_id":"conversation-1",
            "output":format!("> jest\nFAIL src/app.test.js\nNPM_TOKEN={TOKEN}\n")
        }),
    );
    let output = &s.exported_command("npm test")["failure_output"];
    assert_eq!(output["stream"], "output");
    let tail = output["tail"].as_str().unwrap();
    assert!(tail.contains("FAIL src/app.test.js"), "{tail}");
    assert!(!tail.contains("ghp_aBcDeF"), "{tail}");
}

#[test]
fn codex_failures_keep_stdout_then_stderr() {
    let s = Sandbox::capturing();
    let cwd = s.project();
    s.hook(
        "hook-codex",
        &serde_json::json!({
            "hook_event_name":"PostToolUse", "session_id":"session-123", "turn_id":"turn-1",
            "tool_use_id":"call-1", "tool_name":"Bash", "cwd":cwd,
            "tool_input":{"command":"pytest"},
            "tool_response":{"exit_code":1,"stdout":"collected 3 items\n1 failed","stderr":"ERROR: tests failed"}
        }),
    );
    let output = &s.exported_command("pytest")["failure_output"];
    assert_eq!(output["stream"], "stdout+stderr");
    assert_eq!(
        output["tail"],
        "collected 3 items\n1 failed\nERROR: tests failed"
    );
}

#[test]
fn codex_without_an_exit_code_keeps_nothing() {
    let s = Sandbox::capturing();
    let cwd = s.project();
    s.hook(
        "hook-codex",
        &serde_json::json!({
            "hook_event_name":"PostToolUse", "session_id":"session-123", "turn_id":"turn-1",
            "tool_name":"Bash", "cwd":cwd, "tool_input":{"command":"pytest"},
            "tool_response":"Exit code: 1\nOutput:\nfailed"
        }),
    );
    let entry = s.exported_command("pytest");
    assert!(entry["exit_code"].is_null());
    assert!(entry.get("failure_output").is_none(), "{entry}");
}

#[test]
fn add_reads_a_failed_commands_output_from_stdin() {
    let s = Sandbox::capturing();
    let cwd = s.project();
    let cwd = cwd.to_str().unwrap();
    let args = [
        "add",
        "--session-id",
        "opencode-s1",
        "--command",
        "go test ./...",
        "--cwd",
        cwd,
        "--exit-code",
        "1",
        "--started-at",
        "1700000000000",
        "--ended-at",
        "1700000000100",
        "--executor-type",
        "agent",
        "--executor",
        "opencode",
        "--output-stdin",
        "--output-dropped-bytes",
        "500",
    ];
    // The sender cut 500 bytes off the front, so its first line is the
    // end of a longer one and is not kept.
    s.run_with_stdin(&args, "of a line\n--- FAIL: TestParse\nFAIL\n");
    let output = &s.exported_command("go test ./...")["failure_output"];
    assert_eq!(output["stream"], "output");
    assert_eq!(output["tail"], "--- FAIL: TestParse\nFAIL");
    assert_eq!(output["truncated_bytes"], 500 + "of a line\n".len());
}

#[test]
fn a_project_overlay_cannot_turn_capture_on() {
    let s = Sandbox::new();
    let cwd = s.project();
    std::fs::write(
        cwd.join(".suvadu.toml"),
        "[capture]\nfailure_output = true\n",
    )
    .unwrap();
    s.hook(
        "hook-claude-code-failure",
        &claude_failure(&cwd, "make", "Exit code 2\nmake: *** No targets"),
    );
    assert!(s.exported_command("make").get("failure_output").is_none());
}

#[test]
fn a_project_overlay_can_turn_capture_off() {
    let s = Sandbox::capturing();
    let cwd = s.project();
    std::fs::write(
        cwd.join(".suvadu.toml"),
        "[capture]\nfailure_output = false\n",
    )
    .unwrap();
    s.hook(
        "hook-claude-code-failure",
        &claude_failure(&cwd, "make", "Exit code 2\nmake: *** No targets"),
    );
    assert!(s.exported_command("make").get("failure_output").is_none());
}

#[test]
fn an_excluded_command_keeps_no_output() {
    let s = Sandbox::new();
    s.write_config("exclusions = [\"^vault \"]\n[capture]\nfailure_output = true\n");
    let cwd = s.project();
    s.hook(
        "hook-claude-code-failure",
        &claude_failure(&cwd, "vault login", "Exit code 2\npermission denied"),
    );
    assert!(s.exported().is_empty());
}

#[test]
fn deleting_the_command_deletes_its_output_and_gc_can_drop_the_rest() {
    let s = Sandbox::capturing();
    let cwd = s.project();
    for command in ["make", "make test"] {
        s.hook(
            "hook-claude-code-failure",
            &claude_failure(&cwd, command, "Exit code 2\nmake: *** failed"),
        );
    }
    let out = s.run(&["delete", "^make$", "--regex", "--yes", "--no-backup"]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let left = s.exported();
    assert_eq!(left.len(), 1);
    assert!(left[0].get("failure_output").is_some());

    let out = s.run(&["gc", "--failure-output"]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("failure output stored for 1"), "{stdout}");
    let left = s.exported();
    assert_eq!(left.len(), 1, "the command stays");
    assert!(left[0].get("failure_output").is_none());
}

#[cfg(unix)]
#[test]
fn wrap_passes_stderr_through_and_keeps_its_tail_on_failure() {
    let s = Sandbox::capturing();
    let cwd = s.project();
    // The token is assembled by the shell, so only the output carries it.
    let script = format!(
        "echo to-stdout; printf 'progress 10%%\\rprogress 100%%\\n' >&2; \
         t=ghp_; echo \"denied for token ${{t}}{}\" >&2; exit 3",
        &TOKEN[4..]
    );
    let output = s
        .command()
        .current_dir(&cwd)
        .args(["wrap", "--", &script])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(3));
    assert_eq!(String::from_utf8_lossy(&output.stdout), "to-stdout\n");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.starts_with(&format!(
            "progress 10%\rprogress 100%\ndenied for token {TOKEN}\n"
        )),
        "stderr reached the terminal changed: {stderr:?}"
    );
    let entry = s.exported_command(&script);
    let output = &entry["failure_output"];
    assert_eq!(output["stream"], "stderr");
    let tail = output["tail"].as_str().unwrap();
    assert!(
        tail.starts_with("progress 100%\ndenied for token"),
        "{tail}"
    );
    assert!(!tail.contains("ghp_aBcDeF"), "{tail}");
    assert!(!tail.contains("to-stdout"), "only stderr is kept: {tail}");
}

#[cfg(unix)]
#[test]
fn wrap_keeps_nothing_when_the_command_succeeds_or_capture_is_off() {
    for (sandbox, script) in [
        (Sandbox::capturing(), "echo fine >&2"),
        (Sandbox::new(), "echo broken >&2; exit 1"),
    ] {
        let cwd = sandbox.project();
        let output = sandbox
            .command()
            .current_dir(&cwd)
            .args(["wrap", "--", script])
            .output()
            .unwrap();
        assert!(
            String::from_utf8_lossy(&output.stderr).contains("fine")
                || String::from_utf8_lossy(&output.stderr).contains("broken")
        );
        let entry = sandbox.exported_command(script);
        assert!(entry.get("failure_output").is_none(), "{entry}");
    }
}

/// Load the installed `OpenCode` plugin under node and fire
/// `tool.execute.after` for one bash call with `output`.
#[cfg(unix)]
fn run_opencode_tool(s: &Sandbox, command: &str, output: &serde_json::Value) {
    let init = s.run(&["init", "opencode"]);
    assert!(
        init.status.success(),
        "{}",
        String::from_utf8_lossy(&init.stderr)
    );
    let bin = s.home.path().join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    std::os::unix::fs::symlink(env!("CARGO_BIN_EXE_suv"), bin.join("suv")).unwrap();
    let driver = s.home.path().join("driver.mjs");
    std::fs::write(
        &driver,
        r#"import { readFileSync } from "fs";
import { pathToFileURL } from "url";
const [plugin, command, cwd, outputFile] = process.argv.slice(2);
const client = { session: { messages: async () => ({ data: [] }) } };
const hooks = await (await import(pathToFileURL(plugin).href)).default({ client, directory: cwd });
await hooks["tool.execute.after"](
  { tool: "bash", sessionID: "ses_1", args: { command, workdir: cwd } },
  JSON.parse(readFileSync(outputFile, "utf8")),
);
"#,
    )
    .unwrap();
    let cwd = s.project();
    // In a file, not an argument: Linux limits one argument to 128 KB.
    let output_file = s.home.path().join("tool-output.json");
    std::fs::write(&output_file, output.to_string()).unwrap();
    let result = s
        .command()
        .env(
            "PATH",
            format!(
                "{}:{}",
                bin.display(),
                std::env::var("PATH").unwrap_or_default()
            ),
        )
        .arg("--version")
        .output()
        .unwrap();
    assert!(result.status.success());
    let result = Command::new("node")
        .env("HOME", s.home.path())
        .env("XDG_DATA_HOME", s.home.path().join("data"))
        .env("XDG_CONFIG_HOME", s.home.path().join("config"))
        .env_remove("SUVADU_PAUSED")
        .env(
            "PATH",
            format!(
                "{}:{}",
                bin.display(),
                std::env::var("PATH").unwrap_or_default()
            ),
        )
        .arg(&driver)
        .arg(s.home.path().join(".opencode/plugins/suvadu.js"))
        .arg(command)
        .arg(&cwd)
        .arg(&output_file)
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
}

#[cfg(unix)]
#[test]
fn the_opencode_plugin_sends_a_failed_commands_output() {
    if Command::new("node").arg("--version").output().is_err() {
        eprintln!("skipping: node is not installed");
        return;
    }
    let s = Sandbox::capturing();
    let log = format!(
        "{}FAIL src/app.test.ts\nToken {TOKEN} rejected\n",
        "ok\n".repeat(50_000)
    );
    run_opencode_tool(
        &s,
        "bun test",
        &serde_json::json!({"title": "bun test", "output": log, "metadata": {"exit": 1, "output": log}}),
    );
    let output = &s.exported_command("bun test")["failure_output"];
    let tail = output["tail"].as_str().unwrap();
    assert!(tail.ends_with("rejected"), "{tail}");
    assert!(tail.contains("FAIL src/app.test.ts"), "{tail}");
    assert!(!tail.contains("ghp_aBcDeF"), "{tail}");
    // The plugin sent the end of the log and counted the rest (about:
    // redaction changes the length of what is counted after the window).
    let left_out = output["truncated_bytes"].as_i64().unwrap();
    let expected = i64::try_from(log.trim_end().len() - tail.len()).unwrap();
    assert!(
        (left_out - expected).abs() < 100,
        "{left_out} vs {expected}"
    );

    let s = Sandbox::capturing();
    run_opencode_tool(
        &s,
        "bun run build",
        &serde_json::json!({"title": "build", "output": "built", "metadata": {"exit": 0}}),
    );
    assert!(s
        .exported_command("bun run build")
        .get("failure_output")
        .is_none());
}
