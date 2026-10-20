//! Git snapshots of agent sessions, end to end: the Claude Code, Codex and
//! `OpenCode` hooks take one at a session's first prompt and at each turn
//! end, through the real `suv` binary, and never fail the hook when there
//! is nothing to snapshot.
//!
//! Every test runs against a private HOME and temporary git repositories;
//! nothing here reads or writes the user's own database, config or repos.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

use suvadu::ai_sessions::git_snapshot::GitSnapshot;
use suvadu::repository::Repository;

struct Sandbox {
    home: tempfile::TempDir,
    path: Option<std::ffi::OsString>,
}

impl Sandbox {
    fn new() -> Self {
        Self {
            home: tempfile::tempdir().unwrap(),
            path: None,
        }
    }

    /// A sandbox whose hooks run with no `git` on `PATH`.
    fn without_git() -> Self {
        let mut s = Self::new();
        let empty = s.home.path().join("empty-bin");
        std::fs::create_dir_all(&empty).unwrap();
        s.path = Some(empty.into_os_string());
        s
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

    fn db_path(&self) -> PathBuf {
        if cfg!(target_os = "macos") {
            self.home
                .path()
                .join("Library/Application Support/tech.appachi.suvadu/history.db")
        } else {
            self.home.path().join("data/suvadu/history.db")
        }
    }

    fn command(&self) -> Command {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_suv"));
        cmd.env("HOME", self.home.path())
            .env("XDG_DATA_HOME", self.home.path().join("data"))
            .env("XDG_CONFIG_HOME", self.home.path().join("config"))
            .env("CODEX_HOME", self.home.path().join(".codex"))
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env_remove("SUVADU_PAUSED")
            .env_remove("SUVADU_SESSION_ID")
            .env("NO_COLOR", "1");
        if let Some(path) = &self.path {
            cmd.env("PATH", path);
        }
        cmd
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

    fn hook(&self, hook: &str, event: &serde_json::Value) -> Output {
        self.run_with_stdin(&[hook], &event.to_string())
    }

    fn run(&self, args: &[&str]) -> Output {
        self.command().args(args).output().unwrap()
    }

    /// The stored start and end snapshots of `session`.
    fn snapshots(&self, session: &str) -> (Option<GitSnapshot>, Option<GitSnapshot>) {
        if !self.db_path().exists() {
            return (None, None);
        }
        let conn = suvadu::db::init_db(&self.db_path()).unwrap();
        Repository::new(conn).git_snapshots(session).unwrap()
    }

    /// A git repository inside the sandbox with one commit holding `a.txt`.
    fn repo(&self) -> PathBuf {
        let dir = self.home.path().join("project");
        std::fs::create_dir_all(&dir).unwrap();
        git(&dir, &["init", "-q"]);
        std::fs::write(dir.join("a.txt"), "a\n").unwrap();
        std::fs::write(dir.join("old.txt"), "old\n").unwrap();
        git(&dir, &["add", "."]);
        git(&dir, &["commit", "-q", "-m", "first"]);
        dir
    }
}

/// Git for a test repository, isolated from the developer's own git
/// configuration.
fn git(dir: &Path, args: &[&str]) {
    let status = Command::new("git")
        .args([
            "-c",
            "user.name=Suvadu Test",
            "-c",
            "user.email=test@example.invalid",
            "-c",
            "commit.gpgsign=false",
            "-c",
            "init.defaultBranch=main",
        ])
        .arg("-C")
        .arg(dir)
        .args(args)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .unwrap();
    assert!(status.success(), "git {args:?}");
}

fn head(dir: &Path) -> String {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["rev-parse", "HEAD"])
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .output()
        .unwrap();
    String::from_utf8(out.stdout).unwrap().trim().to_string()
}

fn claude_prompt(cwd: &Path) -> serde_json::Value {
    serde_json::json!({
        "hook_event_name":"UserPromptSubmit", "session_id":"session-1",
        "prompt":"tidy the project", "cwd":cwd
    })
}

fn claude_stop(s: &Sandbox, cwd: &Path) -> serde_json::Value {
    serde_json::json!({
        "hook_event_name":"Stop", "session_id":"session-1",
        "transcript_path":s.home.path().join("no-transcript.jsonl"), "cwd":cwd
    })
}

/// The changes a session makes: one commit, a rename, a deletion and an
/// untracked file.
fn make_changes(dir: &Path) {
    std::fs::write(dir.join("a.txt"), "edited\n").unwrap();
    git(dir, &["commit", "-q", "-am", "edit a"]);
    git(dir, &["mv", "old.txt", "new.txt"]);
    std::fs::write(dir.join("fresh.txt"), "fresh\n").unwrap();
}

#[test]
fn claude_code_snapshots_the_first_prompt_and_each_turn_end() {
    let s = Sandbox::new();
    let repo = s.repo();
    let before = head(&repo);
    s.hook("hook-claude-prompt", &claude_prompt(&repo));
    make_changes(&repo);
    // A later prompt keeps the first snapshot.
    s.hook("hook-claude-prompt", &claude_prompt(&repo));
    s.hook("hook-claude-session", &claude_stop(&s, &repo));

    let (start, end) = s.snapshots("claude-session-1");
    let start = start.expect("a start snapshot");
    let end = end.expect("an end snapshot");
    assert_eq!(start.head.as_deref(), Some(before.as_str()));
    assert_eq!(start.branch.as_deref(), Some("main"));
    assert!(start.paths.is_empty(), "{:?}", start.paths);
    assert_eq!(end.head.as_deref(), Some(head(&repo).as_str()));
    let renamed = end.paths.iter().find(|p| p.path == "new.txt").unwrap();
    assert_eq!(renamed.from.as_deref(), Some("old.txt"));
    assert!(end
        .paths
        .iter()
        .any(|p| p.path == "fresh.txt" && p.status == "??"));
    assert!(start.at <= end.at);

    // The next turn end replaces the end snapshot.
    std::fs::remove_file(repo.join("fresh.txt")).unwrap();
    s.hook("hook-claude-session", &claude_stop(&s, &repo));
    let (_, end) = s.snapshots("claude-session-1");
    assert!(!end.unwrap().paths.iter().any(|p| p.path == "fresh.txt"));
}

#[test]
fn a_turn_end_without_a_directory_uses_the_first_prompts() {
    let s = Sandbox::new();
    let repo = s.repo();
    s.hook("hook-claude-prompt", &claude_prompt(&repo));
    let mut stop = claude_stop(&s, &repo);
    stop.as_object_mut().unwrap().remove("cwd");
    s.hook("hook-claude-session", &stop);
    assert!(s.snapshots("claude-session-1").1.is_some());
}

#[test]
fn outside_a_repository_nothing_is_recorded_and_the_hooks_still_succeed() {
    let s = Sandbox::new();
    let plain = s.home.path().join("plain");
    std::fs::create_dir_all(&plain).unwrap();
    s.hook("hook-claude-prompt", &claude_prompt(&plain));
    s.hook("hook-claude-session", &claude_stop(&s, &plain));
    assert_eq!(s.snapshots("claude-session-1"), (None, None));
}

#[test]
fn without_git_on_path_nothing_is_recorded_and_the_hooks_still_succeed() {
    let s = Sandbox::without_git();
    let repo = s.repo();
    s.hook("hook-claude-prompt", &claude_prompt(&repo));
    s.hook("hook-claude-session", &claude_stop(&s, &repo));
    assert_eq!(s.snapshots("claude-session-1"), (None, None));
}

#[test]
fn excluded_paths_are_never_stored_and_secrets_in_paths_are_redacted() {
    let s = Sandbox::new();
    s.write_config("exclusions = [\"private/\"]\n");
    let repo = s.repo();
    s.hook("hook-claude-prompt", &claude_prompt(&repo));
    std::fs::create_dir_all(repo.join("private")).unwrap();
    std::fs::write(repo.join("private/plan.txt"), "x").unwrap();
    let token = "ghp_aBcDeFgHiJkLmNoPqRsTuVwXyZ0123456789";
    std::fs::write(repo.join(format!("{token}.txt")), "x").unwrap();
    s.hook("hook-claude-session", &claude_stop(&s, &repo));
    let end = s.snapshots("claude-session-1").1.unwrap();
    let stored = format!("{end:?}");
    assert!(!stored.contains("private/"), "{stored}");
    assert!(!stored.contains(token), "{stored}");
    assert_eq!(end.omitted, 1);
    assert_eq!(end.paths.len(), 1);
}

#[test]
fn snapshots_can_be_turned_off_but_a_project_cannot_turn_them_back_on() {
    let s = Sandbox::new();
    s.write_config("[agent]\nrecord_git_snapshots = false\n");
    let repo = s.repo();
    std::fs::write(
        repo.join(".suvadu.toml"),
        "[agent]\nrecord_git_snapshots = true\n",
    )
    .unwrap();
    s.hook("hook-claude-prompt", &claude_prompt(&repo));
    s.hook("hook-claude-session", &claude_stop(&s, &repo));
    assert_eq!(s.snapshots("claude-session-1"), (None, None));

    let s = Sandbox::new();
    let repo = s.repo();
    std::fs::write(
        repo.join(".suvadu.toml"),
        "[agent]\nrecord_git_snapshots = false\n",
    )
    .unwrap();
    s.hook("hook-claude-prompt", &claude_prompt(&repo));
    s.hook("hook-claude-session", &claude_stop(&s, &repo));
    assert_eq!(s.snapshots("claude-session-1"), (None, None));
}

#[test]
fn paused_capture_takes_no_snapshot() {
    let s = Sandbox::new();
    let repo = s.repo();
    let mut child = s
        .command()
        .arg("hook-claude-prompt")
        .env("SUVADU_PAUSED", "1")
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(claude_prompt(&repo).to_string().as_bytes())
        .unwrap();
    assert!(child.wait().unwrap().success());
    assert_eq!(s.snapshots("claude-session-1"), (None, None));
}

#[test]
fn codex_snapshots_at_the_prompt_and_at_stop() {
    let s = Sandbox::new();
    let repo = s.repo();
    s.hook(
        "hook-codex",
        &serde_json::json!({"hook_event_name":"UserPromptSubmit", "session_id":"codex-1",
            "turn_id":"t1", "cwd":repo, "prompt":"go"}),
    );
    make_changes(&repo);
    let out = s.hook(
        "hook-codex",
        &serde_json::json!({"hook_event_name":"Stop", "session_id":"codex-1", "cwd":repo,
            "transcript_path":s.home.path().join("none.jsonl")}),
    );
    assert_eq!(out.stdout, b"{}\n", "Stop still answers Codex");
    let (start, end) = s.snapshots("codex-codex-1");
    assert!(start.is_some() && end.is_some());
    assert_ne!(start.unwrap().head, end.unwrap().head);
}

#[test]
fn opencode_snapshots_at_the_prompt_and_when_idle() {
    let s = Sandbox::new();
    let repo = s.repo();
    let dir = repo.to_string_lossy().to_string();
    s.run_with_stdin(
        &[
            "hook-opencode-prompt",
            "--session-id",
            "oc-1",
            "--directory",
            &dir,
        ],
        "go",
    );
    make_changes(&repo);
    s.run_with_stdin(
        &[
            "hook-opencode-session",
            "--session-id",
            "oc-1",
            "--directory",
            &dir,
        ],
        "[]",
    );
    let (start, end) = s.snapshots("opencode-oc-1");
    assert!(start.is_some() && end.is_some());
}

#[test]
fn deleting_the_session_deletes_its_snapshots() {
    let s = Sandbox::new();
    let repo = s.repo();
    s.hook("hook-claude-prompt", &claude_prompt(&repo));
    s.hook("hook-claude-session", &claude_stop(&s, &repo));
    assert!(s.snapshots("claude-session-1").1.is_some());
    let out = s.run(&["agent", "delete-session", "claude-session-1"]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(String::from_utf8_lossy(&out.stdout).contains("2 git snapshot"));
    assert_eq!(s.snapshots("claude-session-1"), (None, None));
}

/// What a snapshot adds to the hooks that take one, in a repository of
/// 10,000 tracked files with 50 modified and 20 untracked. Prints rather
/// than asserts. Run with
/// `cargo test --release --test git_snapshots hook_overhead -- --ignored --nocapture`.
#[test]
#[ignore = "timing, not correctness"]
fn hook_overhead() {
    fn time(s: &Sandbox, hook: &str, event: &str, before: impl Fn()) -> String {
        let mut runs = Vec::new();
        for round in 0..43 {
            before();
            let started = std::time::Instant::now();
            let mut child = s
                .command()
                .arg(hook)
                .stdin(Stdio::piped())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .unwrap();
            child
                .stdin
                .take()
                .unwrap()
                .write_all(event.as_bytes())
                .unwrap();
            child.wait().unwrap();
            if round >= 3 {
                runs.push(started.elapsed());
            }
        }
        runs.sort();
        format!(
            "median {:.1?}, p95 {:.1?}",
            runs[runs.len() / 2],
            runs[runs.len() * 95 / 100 - 1]
        )
    }
    let s = Sandbox::new();
    let repo = s.home.path().join("big");
    std::fs::create_dir_all(&repo).unwrap();
    git(&repo, &["init", "-q"]);
    for i in 0..100 {
        let dir = repo.join(format!("d{i}"));
        std::fs::create_dir_all(&dir).unwrap();
        for j in 0..100 {
            std::fs::write(dir.join(format!("f{j}.txt")), format!("{i} {j}\n")).unwrap();
        }
    }
    git(&repo, &["add", "."]);
    git(&repo, &["commit", "-q", "-m", "init"]);
    for i in 0..50 {
        std::fs::write(repo.join(format!("d{i}/f1.txt")), "changed\n").unwrap();
    }
    for i in 0..20 {
        std::fs::write(repo.join(format!("new{i}.txt")), "new\n").unwrap();
    }
    let plain = s.home.path().join("plain");
    std::fs::create_dir_all(&plain).unwrap();
    let prompt = claude_prompt(&repo).to_string();
    let stop = claude_stop(&s, &repo).to_string();
    let stop_plain = claude_stop(&s, &plain).to_string();
    s.hook("hook-claude-prompt", &claude_prompt(&repo));
    let clear = || {
        let conn = rusqlite::Connection::open(s.db_path()).unwrap();
        conn.execute("DELETE FROM ai_snapshots", []).unwrap();
    };
    let nothing = || {};
    println!(
        "prompt, start already taken: {}",
        time(&s, "hook-claude-prompt", &prompt, nothing)
    );
    println!(
        "prompt, first of a session:  {}",
        time(&s, "hook-claude-prompt", &prompt, clear)
    );
    println!(
        "stop, in the repository:     {}",
        time(&s, "hook-claude-session", &stop, nothing)
    );
    println!(
        "stop, outside a repository:  {}",
        time(&s, "hook-claude-session", &stop_plain, nothing)
    );
    s.write_config("[agent]\nrecord_git_snapshots = false\n");
    println!(
        "prompt, snapshots off:       {}",
        time(&s, "hook-claude-prompt", &prompt, nothing)
    );
    println!(
        "stop, snapshots off:         {}",
        time(&s, "hook-claude-session", &stop, nothing)
    );
}

impl Sandbox {
    /// `what_changed` called over `suv mcp-serve`, as an agent would.
    fn what_changed(&self, args: &serde_json::Value) -> String {
        let mut input = String::new();
        for message in [
            serde_json::json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{
                "protocolVersion":"2025-03-26","capabilities":{},
                "clientInfo":{"name":"t","version":"1"}}}),
            serde_json::json!({"jsonrpc":"2.0","method":"notifications/initialized"}),
            serde_json::json!({"jsonrpc":"2.0","id":2,"method":"tools/call",
                "params":{"name":"what_changed","arguments":args}}),
        ] {
            input.push_str(&message.to_string());
            input.push('\n');
        }
        let output = self.run_with_stdin(&["mcp-serve"], &input);
        let stdout = String::from_utf8(output.stdout).unwrap();
        let reply: serde_json::Value = stdout
            .lines()
            .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
            .find(|message| message["id"] == 2)
            .unwrap();
        reply["result"]["content"][0]["text"]
            .as_str()
            .unwrap_or_else(|| panic!("{reply}"))
            .to_string()
    }
}

#[test]
fn what_changed_reports_a_commit_made_during_the_session_as_observed() {
    let s = Sandbox::new();
    let repo = s.repo();
    s.hook("hook-claude-prompt", &claude_prompt(&repo));
    make_changes(&repo);
    std::fs::remove_file(repo.join("a.txt")).unwrap();
    s.hook("hook-claude-session", &claude_stop(&s, &repo));

    let text = s.what_changed(&serde_json::json!({"session_id":"session-1"}));
    assert!(text.contains("provenance: observed"), "{text}");
    assert!(text.contains("1 commit"), "{text}");
    assert!(text.contains(" edit a"), "{text}");
    assert!(text.contains("deleted | a.txt"), "{text}");
    assert!(text.contains("new.txt (from old.txt)"), "{text}");
    assert!(text.contains("untracked | fresh.txt"), "{text}");

    let window = s.what_changed(&serde_json::json!({"hours":1}));
    assert!(window.contains("claude-session-1"), "{window}");
}

#[test]
fn what_changed_outside_a_repository_is_inferred_and_says_why() {
    let s = Sandbox::new();
    let plain = s.home.path().join("plain");
    std::fs::create_dir_all(&plain).unwrap();
    s.hook("hook-claude-prompt", &claude_prompt(&plain));
    s.hook(
        "hook-claude-code",
        &serde_json::json!({"hook_event_name":"PostToolUse","session_id":"session-1",
            "cwd":plain,"tool_name":"Bash","tool_input":{"command":"touch notes.txt"},
            "tool_response":{"exit_code":0}}),
    );
    s.hook("hook-claude-session", &claude_stop(&s, &plain));
    let text = s.what_changed(&serde_json::json!({"session_id":"claude-session-1"}));
    assert!(text.contains("provenance: observed+inferred"), "{text}");
    assert!(text.contains("no git snapshot was taken"), "{text}");
    assert!(text.contains("FILE WRITES"), "{text}");
}
