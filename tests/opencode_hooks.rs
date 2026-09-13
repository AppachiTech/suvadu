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
    fn cache_prompt(&self, session_id: &str, directory: &str, prompt: &str) {
        self.run_with_stdin(
            &[
                "hook-opencode-prompt",
                "--session-id",
                session_id,
                "--directory",
                directory,
            ],
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

/// Recursively find the first file named exactly `name` under `path`.
fn find_file_named(path: &std::path::Path, name: &str) -> Option<std::path::PathBuf> {
    for entry in std::fs::read_dir(path).unwrap().flatten() {
        let entry_path = entry.path();
        if entry.file_type().unwrap().is_dir() {
            if let Some(found) = find_file_named(&entry_path, name) {
                return Some(found);
            }
        } else if entry_path.file_name().and_then(|n| n.to_str()) == Some(name) {
            return Some(entry_path);
        }
    }
    None
}

#[test]
fn opencode_prompt_hash_is_keyed_and_the_reconciliation_key_persists_across_processes() {
    // A bare, unkeyed hash of the raw prompt would let anyone who can read
    // the database or this sidecar file offline dictionary-guess a
    // redacted low-entropy secret. The fingerprint must be an HMAC keyed
    // by a per-install secret, and that secret must be owner-only and
    // stable across separate `suv` process invocations (the real usage
    // pattern -- every hook is its own process).
    let s = Sandbox::new();
    s.cache_prompt("ses1", "/project", "first prompt, nothing sensitive");

    let hash_file = find_file_named(s.home.path(), "opencode-ses1.prompt.hash")
        .expect("hash sidecar should have been written");
    let hash_contents = std::fs::read_to_string(&hash_file).unwrap();
    assert!(
        hash_contents.starts_with("hmac-sha256:"),
        "prompt hash must be a keyed HMAC, not a bare unkeyed hash: {hash_contents}"
    );

    let key_file =
        find_file_named(s.home.path(), "reconcile.key").expect("reconciliation key should exist");
    let key_bytes_first = std::fs::read(&key_file).unwrap();
    assert_eq!(key_bytes_first.len(), 32);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            key_file.metadata().unwrap().permissions().mode() & 0o777,
            0o600,
            "the reconciliation key must be owner-only"
        );
    }

    // A second, separate `suv` process must reuse the same persisted key
    // rather than generating a fresh one.
    s.cache_prompt("ses2", "/project", "second prompt, different session");
    let key_bytes_second = std::fs::read(&key_file).unwrap();
    assert_eq!(
        key_bytes_first, key_bytes_second,
        "the reconciliation key must persist across separate process invocations"
    );
}

#[test]
#[allow(clippy::needless_collect)] // must spawn every process before waiting on any
fn opencode_concurrent_first_use_key_creation_is_race_safe() {
    // On a fresh install, many hook invocations can race to create
    // reconcile.key for the first time. This end-to-end version, racing
    // real separate `suv` processes, confirms the deployed binary behaves
    // correctly under concurrent first use (every process gets a valid,
    // consistent, owner-only key) but real process spawn overhead
    // (milliseconds) dwarfs the actual race window (a syscall or two), so
    // it won't reliably reproduce a create-then-write/rename-clobber
    // regression by itself -- see
    // `concurrent_first_use_never_misses_a_key_or_disagrees_on_it` in
    // src/util/reconcile_key.rs, which packs synchronized in-process
    // threads tightly enough to actually catch that (reliably reproduced
    // against the prior implementation before this fix).
    let s = Sandbox::new();
    let n = 8;
    let results: Vec<std::process::Output> = std::thread::scope(|scope| {
        let handles: Vec<_> = (0..n)
            .map(|i| {
                let s = &s;
                scope.spawn(move || {
                    s.run_with_stdin(
                        &[
                            "hook-opencode-prompt",
                            "--session-id",
                            &format!("ses{i}"),
                            "--directory",
                            "/project",
                        ],
                        &format!("concurrent prompt number {i}"),
                    )
                })
            })
            .collect();
        handles.into_iter().map(|h| h.join().unwrap()).collect()
    });
    assert_eq!(results.len(), n);

    // Every hash sidecar must exist and be a keyed HMAC, never missing or
    // garbage from a partially-written key file caught mid-race.
    for i in 0..n {
        let hash_file = find_file_named(s.home.path(), &format!("opencode-ses{i}.prompt.hash"))
            .unwrap_or_else(|| panic!("hash sidecar for ses{i} should exist despite the race"));
        let contents = std::fs::read_to_string(&hash_file).unwrap();
        assert!(
            contents.starts_with("hmac-sha256:"),
            "ses{i} hash was not a valid keyed HMAC: {contents}"
        );
    }

    // Exactly one reconciliation key must have won the race, and it must
    // be owner-only from the moment it became visible.
    let key_file = find_file_named(s.home.path(), "reconcile.key")
        .expect("a reconciliation key should have been created");
    assert_eq!(std::fs::read(&key_file).unwrap().len(), 32);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            key_file.metadata().unwrap().permissions().mode() & 0o777,
            0o600,
            "the reconciliation key must be owner-only, with no window at any other mode"
        );
    }
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

    s.cache_prompt("ses1", "/project", &prompt);
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
    s.cache_prompt("ses1", "/project", &prompt);
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

#[test]
fn opencode_prompt_cache_respects_a_project_suvadu_toml_overlay() {
    let s = Sandbox::new();
    let project = s.home.path().join("project");
    std::fs::create_dir_all(&project).unwrap();
    // A project overlay that diverges from the global defaults (redaction
    // on, 4000-char cap): shrinks the cap (well past where the secret sits)
    // and turns redaction off. Before routing the prompt cache through
    // load_config_for_dir(directory), the cache always used the global
    // config, so this project's own policy would have been ignored for the
    // cached copy.
    std::fs::write(
        project.join(".suvadu.toml"),
        "[agent]\nprompt_capture_max_chars = 200\n[redaction]\nenabled = false\n",
    )
    .unwrap();
    let project_dir = project.to_string_lossy().to_string();

    let secret = "overlay_secret_should_survive_here";
    let prompt = format!("token={secret} {}", "padding ".repeat(40));
    assert!(prompt.len() > 300, "prompt must exceed the overlay's cap");

    s.cache_prompt("ses1", &project_dir, &prompt);
    s.add_command("opencode-ses1", "echo overlay-check", &project_dir);

    let entries = s.history();
    let stored = entries[0]["context"]["agent_prompt"].as_str().unwrap();
    // The overlay turns redaction off for this project, unlike the global
    // default, so the secret (well within the 200-char cap) is expected to
    // survive here...
    assert!(
        stored.contains(secret),
        "project overlay disabling redaction was not applied to the cache: {stored}"
    );
    // ...but its much shorter max_chars must still apply, not the 4000-char
    // global default (which would keep the whole prompt).
    assert!(
        stored.len() < prompt.len() && stored.len() < 300,
        "expected the project overlay's 200-char cap, not the global default: {stored}"
    );

    // Session import already resolved this same directory's config before
    // this fix. The two independently-processed copies must still match
    // for reconcile_agent_command_turns to link them.
    let messages = serde_json::json!([user_message("msg_u1", &prompt)]);
    s.import_session("ses1", &project_dir, &messages);
    let entries = s.history();
    assert_eq!(
        entries[0]["context"]["agent_turn_id"], "msg_u1",
        "cache and import must resolve the same project overlay to reconcile"
    );
}

#[test]
fn opencode_command_in_a_stricter_child_directory_stays_redacted_and_still_links_to_its_turn() {
    // The prompt is cached and the session is imported using OpenCode's
    // single root directory (permissive here), but the command that used
    // this prompt actually ran in a stricter child directory. The command's
    // own re-redaction (entry.rs) must still scrub the secret, even though
    // that now makes its agent_prompt text differ from the (unredacted,
    // root-policy) imported prompt event -- reconciliation must bridge that
    // gap by content hash rather than requiring the two texts to match
    // verbatim.
    let s = Sandbox::new();
    let root = s.home.path().join("project");
    let child = root.join("child");
    std::fs::create_dir_all(&child).unwrap();
    std::fs::write(root.join(".suvadu.toml"), "[redaction]\nenabled = false\n").unwrap();
    std::fs::write(child.join(".suvadu.toml"), "[redaction]\nenabled = true\n").unwrap();
    let root_dir = root.to_string_lossy().to_string();
    let child_dir = child.to_string_lossy().to_string();

    let secret = "nested_child_secret_should_not_survive_here";
    let prompt = format!("curl --password={secret} do the thing");

    // Cached at the permissive root: the secret is left untouched here,
    // matching root's own (lax) policy.
    s.cache_prompt("ses1", &root_dir, &prompt);
    // But the command itself ran in the stricter child directory.
    s.add_command("opencode-ses1", "echo secret-check", &child_dir);

    // Session import also resolves the permissive root's policy, so this
    // event's own text legitimately still contains the secret.
    let messages = serde_json::json!([user_message("msg_u1", &prompt)]);
    s.import_session("ses1", &root_dir, &messages);

    let entries = s.history();
    let context = &entries[0]["context"];
    let stored = context["agent_prompt"].as_str().unwrap();
    assert!(
        !stored.contains(secret),
        "the command's own stricter child-directory config must still redact: {stored}"
    );
    assert_eq!(
        context["agent_turn_id"], "msg_u1",
        "differing redaction policies between the command's directory and \
         the session root must not prevent linking to the imported turn; context was: {context}"
    );
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
