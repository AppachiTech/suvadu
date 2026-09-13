//! End-to-end tests for the `OpenCode` prompt cache and its interaction with
//! session-history import. Uses a private home and real CLI/database, same
//! approach as `codex_hooks.rs`.
use std::io::Write;
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
            .env_remove("SUVADU_PAUSED")
            .env("NO_COLOR", "1");
        cmd
    }
    fn run(&self, args: &[&str]) -> Output {
        self.command().args(args).output().unwrap()
    }
    fn run_with_stdin(&self, args: &[&str], stdin_data: &str) -> Output {
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
            .write_all(stdin_data.as_bytes())
            .unwrap();
        let result = child.wait_with_output().unwrap();
        assert!(
            result.status.success(),
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
        result
    }
    fn cache_prompt(&self, session_id: &str, prompt: &str) {
        self.run_with_stdin(
            &["hook-opencode-prompt", "--session-id", session_id],
            prompt,
        );
    }
    fn import_session(&self, session_id: &str, directory: &str, messages: &serde_json::Value) {
        self.run_with_stdin(
            &[
                "hook-opencode-session",
                "--session-id",
                session_id,
                "--directory",
                directory,
            ],
            &messages.to_string(),
        );
    }
    fn add_command(&self, full_session_id: &str, command: &str, cwd: &str) {
        let result = self.run(&[
            "add",
            "--session-id",
            full_session_id,
            "--command",
            command,
            "--cwd",
            cwd,
            "--started-at",
            "1000",
            "--ended-at",
            "1000",
            "--executor-type",
            "agent",
            "--executor",
            "opencode",
        ]);
        assert!(
            result.status.success(),
            "{}",
            String::from_utf8_lossy(&result.stderr)
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
}

fn user_message(id: &str, text: &str) -> serde_json::Value {
    serde_json::json!({
        "info": {"id": id, "sessionID": "ses1", "role": "user", "time": {"created": 1000}},
        "parts": [{"type": "text", "text": text}]
    })
}

/// A prompt over the plugin's old hardcoded 500-char cache cutoff, with a
/// secret placed well before character 500 — before the fix, the plugin
/// wrote this straight to disk with no redaction at all, so a secret at
/// this position would have leaked into the cache and into the command's
/// `agent_prompt` context field verbatim.
fn long_prompt_with_early_secret(secret: &str) -> String {
    format!(
        "Investigate this failure: curl --password={secret} against the staging API. {}",
        "Additional context so the whole prompt exceeds five hundred characters in length. "
            .repeat(6)
    )
}

/// Recursively find `.prompt` cache files under `path`, asserting each is
/// owner-only and does not contain `secret`. Walks the whole sandbox home
/// rather than a hardcoded subpath since the actual data dir is
/// platform-specific (`directories`' `ProjectDirs`, not `XDG_DATA_HOME` on
/// macOS).
fn find_prompt_caches(path: &std::path::Path, secret: &str) -> Vec<String> {
    let mut found = Vec::new();
    for entry in std::fs::read_dir(path).unwrap().flatten() {
        if entry.file_type().unwrap().is_dir() {
            found.extend(find_prompt_caches(&entry.path(), secret));
        } else if entry.path().extension().is_some_and(|ext| ext == "prompt") {
            let contents = std::fs::read_to_string(entry.path()).unwrap();
            assert!(
                !contents.contains(secret),
                "cache leaked secret: {contents}"
            );
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                assert_eq!(
                    entry.metadata().unwrap().permissions().mode() & 0o777,
                    0o600
                );
            }
            found.push(contents);
        }
    }
    found
}

#[test]
fn opencode_prompt_cache_is_redacted_and_not_hard_cut_at_500_chars() {
    let s = Sandbox::new();
    let secret = "sample_password_for_opencode_regression";
    let prompt = long_prompt_with_early_secret(secret);
    assert!(
        prompt.len() > 500,
        "test prompt must exceed the old hardcoded cache cutoff"
    );

    s.cache_prompt("ses1", &prompt);
    s.add_command("opencode-ses1", "echo redaction-check", "/project");

    let entries = s.history();
    let stored = entries[0]["context"]["agent_prompt"].as_str().unwrap();
    assert!(
        stored.contains("REDACTED"),
        "prompt cache must redact secrets like every other agent integration: {stored}"
    );
    assert!(!stored.contains(secret));
    assert!(
        stored.len() > 500,
        "prompt must not be silently cut at the old hardcoded 500-char cap: {stored}"
    );

    let caches = find_prompt_caches(s.home.path(), secret);
    assert_eq!(caches.len(), 1);
}

#[test]
fn opencode_session_import_links_a_command_to_a_prompt_over_500_chars() {
    let s = Sandbox::new();
    let secret = "another_secret_for_opencode_regression";
    let prompt = long_prompt_with_early_secret(secret);
    assert!(prompt.len() > 500);

    // Live path: the plugin caches the prompt as it's typed, then a bash
    // command executes and picks up the cached (redacted) prompt as context.
    s.cache_prompt("ses1", &prompt);
    s.add_command("opencode-ses1", "echo redaction-check", "/project");

    // Later: session.idle fires and the plugin imports the full session
    // history, including the same prompt text OpenCode's own API returns
    // (raw, unredacted — Suvadu redacts it on import).
    let messages = serde_json::json!([user_message("msg_u1", &prompt)]);
    s.import_session("ses1", "/project", &messages);

    let entries = s.history();
    let context = &entries[0]["context"];
    assert_eq!(
        context["agent_turn_id"], "msg_u1",
        "a prompt over 500 chars must still reconcile to its command; context was: {context}"
    );
    let stored = context["agent_prompt"].as_str().unwrap();
    assert!(!stored.contains(secret));
}

fn plugin_path(s: &Sandbox) -> std::path::PathBuf {
    s.home.path().join(".opencode/plugins/suvadu.js")
}
fn opencode_config_path(s: &Sandbox) -> std::path::PathBuf {
    s.home.path().join(".config/opencode/opencode.jsonc")
}

#[test]
fn suv_init_opencode_writes_the_fixed_plugin_and_registers_it_once() {
    let s = Sandbox::new();

    let first = s.run(&["init", "opencode"]);
    assert!(
        first.status.success(),
        "{}",
        String::from_utf8_lossy(&first.stderr)
    );

    let script = std::fs::read_to_string(plugin_path(&s)).unwrap();
    // The generated plugin must be the redaction-safe version, not the old
    // one that wrote its own truncated, unredacted cache file directly.
    assert!(script.contains("hook-opencode-prompt"));
    assert!(!script.contains("writeFileSync"));

    let config: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(opencode_config_path(&s)).unwrap()).unwrap();
    let plugins = config["plugin"].as_array().unwrap();
    let dir = s
        .home
        .path()
        .join(".opencode/plugins")
        .to_string_lossy()
        .to_string();
    assert_eq!(
        plugins
            .iter()
            .filter(|v| v.as_str() == Some(dir.as_str()))
            .count(),
        1
    );

    // Re-running init (e.g. on upgrade) must not duplicate the registration.
    let second = s.run(&["init", "opencode"]);
    assert!(second.status.success());
    let config2: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(opencode_config_path(&s)).unwrap()).unwrap();
    assert_eq!(config2["plugin"].as_array().unwrap().len(), 1);
}

#[test]
fn suv_init_opencode_does_not_corrupt_a_jsonc_config_it_cannot_parse() {
    let s = Sandbox::new();
    let config_path = opencode_config_path(&s);
    std::fs::create_dir_all(config_path.parent().unwrap()).unwrap();
    let jsonc_with_comments =
        "{\n  // a user comment plain JSON can't round-trip\n  \"plugin\": []\n}\n";
    std::fs::write(&config_path, jsonc_with_comments).unwrap();

    let result = s.run(&["init", "opencode"]);
    assert!(
        result.status.success(),
        "init must still succeed overall on a best-effort config-registration failure: {}",
        String::from_utf8_lossy(&result.stderr)
    );
    // The plugin file itself is still installed even though config registration failed.
    assert!(plugin_path(&s).exists());
    // The unparseable config is left byte-for-byte untouched rather than risking corruption.
    assert_eq!(
        std::fs::read_to_string(&config_path).unwrap(),
        jsonc_with_comments
    );
    let stdout = String::from_utf8(result.stdout).unwrap();
    assert!(stdout.contains("Could not update opencode.jsonc automatically"));
}
