//! The generated shell hooks, running inside real interactive shells.
//!
//! Each test starts an interactive shell with a private HOME whose rc file
//! loads the hook with `eval "$(suv init <shell>)"`, and types lines into
//! the shell's stdin. Whatever the hook records goes into that
//! HOME's database, which is read back with `suv history --json` — the
//! user's own shell config and history are never touched.
#![cfg(unix)]

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::time::{Duration, Instant};

/// Give up on a shell long before the harness itself is killed.
const RUN_TIMEOUT: Duration = Duration::from_secs(30);

/// One recorded history row.
#[derive(Debug)]
struct Recorded {
    command: String,
    cwd: String,
    exit_code: Option<i64>,
    duration_ms: i64,
    git_branch: Option<String>,
}

struct Sandbox {
    _root: tempfile::TempDir,
    home: PathBuf,
}

impl Sandbox {
    fn new() -> Self {
        Self::with_home_named("home")
    }

    /// A sandbox whose HOME directory has the given name, for paths that
    /// need quoting.
    fn with_home_named(name: &str) -> Self {
        let root = tempfile::tempdir().unwrap();
        let home = root.path().join(name);
        std::fs::create_dir_all(&home).unwrap();
        Self { _root: root, home }
    }

    fn path(&self, name: &str) -> PathBuf {
        self.home.join(name)
    }

    fn suv(&self) -> Command {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_suv"));
        self.isolate(&mut cmd);
        cmd
    }

    /// Point a command at the sandbox and nothing else.
    fn isolate(&self, cmd: &mut Command) {
        let bin_dir = Path::new(env!("CARGO_BIN_EXE_suv")).parent().unwrap();
        cmd.env_clear()
            .env("HOME", &self.home)
            .env("XDG_DATA_HOME", self.path("data"))
            .env("XDG_CONFIG_HOME", self.path("config"))
            .env(
                "PATH",
                format!("{}:/usr/bin:/bin:/usr/sbin:/sbin", bin_dir.display()),
            )
            .env("TERM", "dumb")
            .env("NO_COLOR", "1");
    }

    /// The rc-file line that loads the hook, as `suv init` documents it.
    /// (`suv` resolves to the binary under test through the sandbox PATH.)
    fn load_hook_line(shell: &str) -> String {
        format!("eval \"$(suv init {shell} 2>/dev/null)\"")
    }

    /// Start `shell_bin` as an interactive shell whose rc file runs
    /// `rc_before`, loads the hook the way `suv init` tells users to, then
    /// runs `rc_after`; then type `lines` at its prompt and let it exit at
    /// end of input.
    fn run_interactive(
        &self,
        kind: ShellKind,
        shell_bin: &Path,
        rc_before: &str,
        rc_after: &str,
        lines: &str,
    ) -> Output {
        let rc = format!(
            "{rc_before}\n{}\n{rc_after}\n",
            Self::load_hook_line(kind.name())
        );
        let mut cmd = Command::new(shell_bin);
        self.isolate(&mut cmd);
        // A fixed session ID, so the hook doesn't depend on `uuidgen` being
        // installed on the machine running the tests.
        cmd.env("SUVADU_SESSION_ID", "shell-hook-test");
        match kind {
            ShellKind::Bash => {
                let rc_path = self.path("bashrc");
                std::fs::write(&rc_path, format!("PS1='$ '\n{rc}")).unwrap();
                cmd.arg("--noprofile")
                    .arg("--rcfile")
                    .arg(&rc_path)
                    .arg("-i")
                    .env("HISTFILE", self.path(".bash_history"));
            }
            ShellKind::Zsh => {
                let zdot = self.path("zdot");
                std::fs::create_dir_all(&zdot).unwrap();
                std::fs::write(zdot.join(".zshrc"), format!("PS1='$ '\n{rc}")).unwrap();
                // -d: skip /etc/zsh* so only the sandbox rc file runs.
                cmd.arg("-d")
                    .arg("-i")
                    .env("ZDOTDIR", &zdot)
                    .env("HISTFILE", self.path(".zsh_history"));
            }
        }
        cmd.current_dir(&self.home)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut child = cmd.spawn().unwrap();
        child
            .stdin
            .take()
            .unwrap()
            .write_all(lines.as_bytes())
            .unwrap();
        let started = Instant::now();
        while child.try_wait().unwrap().is_none() {
            if started.elapsed() > RUN_TIMEOUT {
                let _ = child.kill();
                panic!(
                    "{} did not exit within {RUN_TIMEOUT:?}",
                    shell_bin.display()
                );
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        child.wait_with_output().unwrap()
    }

    fn bash(&self, bash: &Path, rc_before: &str, rc_after: &str, lines: &str) -> Output {
        self.run_interactive(ShellKind::Bash, bash, rc_before, rc_after, lines)
    }

    fn zsh(&self, zsh: &Path, rc_before: &str, rc_after: &str, lines: &str) -> Output {
        self.run_interactive(ShellKind::Zsh, zsh, rc_before, rc_after, lines)
    }

    /// Everything recorded so far, oldest first.
    fn recorded(&self) -> Vec<Recorded> {
        let out = self
            .suv()
            .args(["history", "--json", "-n", "500"])
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "suv history failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        let mut rows: Vec<(i64, Recorded)> = String::from_utf8(out.stdout)
            .unwrap()
            .lines()
            .filter(|l| !l.trim().is_empty())
            .flat_map(|l| {
                let v: serde_json::Value = serde_json::from_str(l).unwrap();
                match v {
                    serde_json::Value::Array(items) => items,
                    other => vec![other],
                }
            })
            .map(|v| {
                let started = v["started_at"].as_i64().unwrap();
                let ended = v["ended_at"].as_i64().unwrap();
                (
                    v["id"].as_i64().unwrap_or(started),
                    Recorded {
                        command: v["command"].as_str().unwrap().to_string(),
                        cwd: v["cwd"].as_str().unwrap().to_string(),
                        git_branch: v["context"]["git_branch"].as_str().map(String::from),
                        exit_code: v["exit_code"].as_i64(),
                        duration_ms: ended - started,
                    },
                )
            })
            .collect();
        rows.sort_by_key(|(id, _)| *id);
        rows.into_iter().map(|(_, r)| r).collect()
    }

    fn recorded_commands(&self) -> Vec<String> {
        self.recorded().into_iter().map(|r| r.command).collect()
    }
}

#[derive(Clone, Copy)]
enum ShellKind {
    Bash,
    Zsh,
}

impl ShellKind {
    const fn name(self) -> &'static str {
        match self {
            Self::Bash => "bash",
            Self::Zsh => "zsh",
        }
    }
}

/// `(major, minor)` of a bash binary, or `None` if it can't be run.
fn bash_version(bin: &Path) -> Option<(u32, u32)> {
    let out = Command::new(bin)
        .args([
            "--norc",
            "--noprofile",
            "-c",
            "echo ${BASH_VERSINFO[0]} ${BASH_VERSINFO[1]}",
        ])
        .output()
        .ok()?;
    let text = String::from_utf8(out.stdout).ok()?;
    let mut parts = text.split_whitespace().map(|p| p.parse::<u32>().ok());
    Some((parts.next()??, parts.next()??))
}

fn bash_candidates() -> Vec<PathBuf> {
    [
        "/opt/homebrew/bin/bash",
        "/usr/local/bin/bash",
        "/bin/bash",
        "/usr/bin/bash",
    ]
    .iter()
    .map(PathBuf::from)
    .filter(|p| p.exists())
    .collect()
}

/// A bash new enough for the hook (4.4+), if this machine has one.
fn modern_bash() -> Option<PathBuf> {
    let found = bash_candidates()
        .into_iter()
        .find(|p| bash_version(p).is_some_and(|v| v >= (4, 4)));
    if found.is_none() {
        eprintln!("skipping: no bash 4.4+ found");
    }
    found
}

/// A bash older than 4.0 (macOS ships 3.2 as /bin/bash), if present.
fn legacy_bash() -> Option<PathBuf> {
    bash_candidates()
        .into_iter()
        .find(|p| bash_version(p).is_some_and(|v| v < (4, 0)))
}

fn zsh_bin() -> Option<PathBuf> {
    let found = [
        "/bin/zsh",
        "/usr/bin/zsh",
        "/opt/homebrew/bin/zsh",
        "/usr/local/bin/zsh",
    ]
    .iter()
    .map(PathBuf::from)
    .find(|p| p.exists());
    if found.is_none() {
        eprintln!("skipping: zsh not found");
    }
    found
}

fn assert_clean(out: &Output) {
    let stderr = String::from_utf8_lossy(&out.stderr);
    // Without a terminal the shell echoes prompts and typed lines to stderr;
    // only its own diagnostics count.
    for line in stderr.lines() {
        // Job-control notices are what any shell without a terminal prints.
        if line.contains("job control") || line.contains("terminal process group") {
            continue;
        }
        let diagnostic = line.starts_with("bash:")
            || line.starts_with("zsh:")
            || line.contains("(eval):")
            || line.contains("syntax error")
            || line.contains("bad substitution")
            || line.contains("bad math expression")
            || line.contains("command not found")
            || line.contains("unexpected EOF");
        assert!(
            !diagnostic,
            "shell printed an error: {line}\nfull stderr:\n{stderr}"
        );
    }
}

// ── bash: what gets recorded ───────────────────────────────

#[test]
fn bash_records_the_whole_line_for_lists_pipelines_and_loops() {
    let Some(bash) = modern_bash() else { return };
    let s = Sandbox::new();
    let out = s.bash(
        &bash,
        "",
        "",
        "cd /tmp && ls >/dev/null\necho a | grep -q b\nfor i in 1 2; do true; done\n",
    );
    assert_clean(&out);
    let rows = s.recorded();
    let commands: Vec<&str> = rows.iter().map(|r| r.command.as_str()).collect();
    assert_eq!(
        commands,
        [
            "cd /tmp && ls >/dev/null",
            "echo a | grep -q b",
            "for i in 1 2; do true; done",
        ]
    );
    assert_eq!(rows[1].exit_code, Some(1), "pipeline exit status is grep's");
}

#[test]
fn bash_times_the_whole_line_not_its_last_command() {
    let Some(bash) = modern_bash() else { return };
    let s = Sandbox::new();
    let out = s.bash(&bash, "", "", "sleep 0.4 && true\n");
    assert_clean(&out);
    let rows = s.recorded();
    assert_eq!(rows.len(), 1, "{rows:?}");
    assert!(
        rows[0].duration_ms >= 350,
        "duration should cover the sleep, got {}ms",
        rows[0].duration_ms
    );
}

#[test]
fn bash_empty_enter_records_nothing() {
    let Some(bash) = modern_bash() else { return };
    let s = Sandbox::new();
    // Another tool's PROMPT_COMMAND entry runs after suvadu's on every prompt.
    let out = s.bash(
        &bash,
        "PROMPT_COMMAND='true'",
        "",
        "echo one\n\n\n(echo sub)\n\necho two\n",
    );
    assert_clean(&out);
    assert_eq!(
        s.recorded_commands(),
        ["echo one", "(echo sub)", "echo two"]
    );
}

#[test]
fn bash_before_4_4_tells_prompt_command_apart_without_prompt_expansion() {
    let Some(bash) = modern_bash() else { return };
    let s = Sandbox::new();
    // bash 4.0-4.3 has no ${var@P}; the hook then relies on
    // __suvadu_prompt_end alone to know PROMPT_COMMAND has finished.
    let out = s.bash(
        &bash,
        "PROMPT_COMMAND='true'",
        "_SUVADU_HAS_CMDNUM=",
        "echo one && true\n\n\nsleep 0.3 | true\n",
    );
    assert_clean(&out);
    let rows = s.recorded();
    let commands: Vec<&str> = rows.iter().map(|r| r.command.as_str()).collect();
    assert_eq!(commands, ["echo one && true", "sleep 0.3 | true"]);
    assert!(rows[1].duration_ms >= 250, "{rows:?}");
    assert!(rows[1].duration_ms < 5000, "{rows:?}");
}

#[test]
fn bash_skips_commands_typed_with_a_leading_space() {
    let Some(bash) = modern_bash() else { return };
    for histcontrol in ["", "ignorespace", "ignoreboth"] {
        let s = Sandbox::new();
        let out = s.bash(
            &bash,
            &format!("HISTCONTROL={histcontrol}"),
            "",
            "echo before\n echo secret\necho after\n",
        );
        assert_clean(&out);
        assert_eq!(
            s.recorded_commands(),
            ["echo before", "echo after"],
            "HISTCONTROL={histcontrol}"
        );
    }
}

#[test]
fn bash_keeps_repeated_commands_under_ignoredups() {
    let Some(bash) = modern_bash() else { return };
    for histcontrol in ["ignoredups", "ignoreboth", "erasedups"] {
        let s = Sandbox::new();
        let out = s.bash(
            &bash,
            &format!("HISTCONTROL={histcontrol}"),
            "",
            "echo same && true\necho same && true\nfalse\nfalse\n",
        );
        assert_clean(&out);
        let rows = s.recorded();
        let commands: Vec<&str> = rows.iter().map(|r| r.command.as_str()).collect();
        assert_eq!(
            commands,
            ["echo same && true", "echo same && true", "false", "false"],
            "HISTCONTROL={histcontrol}"
        );
        assert_eq!(rows[3].exit_code, Some(1));
    }
}

#[test]
fn bash_falls_back_to_the_first_command_when_history_is_off() {
    let Some(bash) = modern_bash() else { return };
    let s = Sandbox::new();
    let out = s.bash(
        &bash,
        "",
        "",
        "set +o history\ncd /tmp && true\n\nset -o history\necho on\n",
    );
    assert_clean(&out);
    assert_eq!(
        s.recorded_commands(),
        ["set +o history", "cd /tmp", "set -o history", "echo on"]
    );
}

#[test]
fn bash_ignores_rc_file_commands_before_the_first_prompt() {
    let Some(bash) = modern_bash() else { return };
    let s = Sandbox::new();
    let out = s.bash(&bash, "", "echo from-rc >/dev/null\ntrue", "echo typed\n");
    assert_clean(&out);
    assert_eq!(s.recorded_commands(), ["echo typed"]);
}

#[test]
fn bash_records_suv_search_not_the_command_it_queued() {
    let Some(bash) = modern_bash() else { return };
    let s = Sandbox::new();
    // A stand-in binary: `search` "selects" a command, everything else is
    // the real suv.
    let fake = s.path("fake-suv");
    std::fs::write(
        &fake,
        format!(
            "#!/bin/sh\nif [ \"$1\" = search ]; then echo 'echo picked'; exit 0; fi\nexec '{}' \"$@\"\n",
            env!("CARGO_BIN_EXE_suv")
        ),
    )
    .unwrap();
    std::fs::set_permissions(&fake, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
    let out = s.bash(
        &bash,
        "",
        &format!("_SUVADU_BIN='{}'", fake.display()),
        "suv search >/dev/null\n",
    );
    assert_clean(&out);
    assert_eq!(s.recorded_commands(), ["suv search > /dev/null"]);
}

#[test]
fn bash_older_than_4_disables_the_hook_quietly() {
    let Some(bash) = legacy_bash() else {
        eprintln!("skipping: no bash older than 4.0 found");
        return;
    };
    let s = Sandbox::new();
    let out = s.bash(&bash, "", "", "echo typed\n");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("Bash 4.0+ required"), "{stderr}");
    assert!(
        !stderr.contains("syntax error") && !stderr.contains("bad substitution"),
        "{stderr}"
    );
    assert!(s.recorded_commands().is_empty());
}

#[test]
fn bash_shared_history_does_not_record_other_shells_lines() {
    let Some(bash) = modern_bash() else { return };
    // The usual "share history between shells" setups: every prompt reads
    // other shells' lines from HISTFILE into this shell's history list.
    for prompt_command in ["history -n", "history -a; history -c; history -r"] {
        let s = Sandbox::new();
        let out = s.bash(
            &bash,
            &format!(
                "HISTCONTROL=ignoreboth\nshopt -s histappend\nPROMPT_COMMAND='{prompt_command}'"
            ),
            "",
            "echo mine\n\
             printf '%s\\n' 'echo from-another-shell' >> \"$HISTFILE\"\n \
             echo secret\n\
             \n\
             echo mine-again\n",
        );
        assert_clean(&out);
        assert_eq!(
            s.recorded_commands(),
            [
                "echo mine",
                "printf '%s\\n' 'echo from-another-shell' >> \"$HISTFILE\"",
                "echo mine-again",
            ],
            "PROMPT_COMMAND={prompt_command}"
        );
    }
}

// ── bash: living alongside other DEBUG traps ───────────────

#[test]
fn bash_keeps_an_existing_multiline_debug_trap_with_quotes() {
    let Some(bash) = modern_bash() else { return };
    let s = Sandbox::new();
    // A trap with quotes of every kind, spread over two lines.
    let out = s.bash(
        &bash,
        r#"trap -- 'printf "%s\n" "it'\''s \"quoted\" $BASH_COMMAND" >> "$HOME/trap.log"
: "second line"' DEBUG"#,
        "",
        "echo hi && true\n",
    );
    assert_clean(&out);
    assert_eq!(s.recorded_commands(), ["echo hi && true"]);
    let log = std::fs::read_to_string(s.path("trap.log")).unwrap_or_default();
    assert!(
        log.lines().any(|l| l == r#"it's "quoted" echo hi"#),
        "existing trap stopped running as written:\n{log}"
    );
}

#[test]
fn bash_loading_the_hook_twice_installs_it_once() {
    let Some(bash) = modern_bash() else { return };
    let s = Sandbox::new();
    let out = s.bash(
        &bash,
        "trap -- ': mine' DEBUG",
        &format!(
            "{}\ntrap -p DEBUG > \"$HOME/trap.txt\"",
            Sandbox::load_hook_line("bash")
        ),
        "echo once\n",
    );
    assert_clean(&out);
    assert_eq!(s.recorded_commands(), ["echo once"]);
    let trap = std::fs::read_to_string(s.path("trap.txt")).unwrap();
    assert_eq!(trap.matches("__suvadu_preexec").count(), 1, "{trap}");
    assert_eq!(trap.matches(": mine").count(), 1, "{trap}");
}

// ── zsh: the hook under the user's own options ─────────────

/// Put a working `suv` at a path with a space in it.
fn suv_at_spaced_path(s: &Sandbox) -> PathBuf {
    let dir = s.path("my tools");
    std::fs::create_dir_all(&dir).unwrap();
    let link = dir.join("suv");
    std::os::unix::fs::symlink(env!("CARGO_BIN_EXE_suv"), &link).unwrap();
    link
}

#[test]
fn zsh_records_under_options_that_change_word_splitting_and_math() {
    let Some(zsh) = zsh_bin() else { return };
    for (when, options) in [
        ("before", "setopt OCTAL_ZEROES SH_WORD_SPLIT NO_UNSET"),
        ("after", ""),
    ] {
        let s = Sandbox::new();
        let bin = suv_at_spaced_path(&s);
        // A clock reading whose milliseconds start with 0, which
        // OCTAL_ZEROES would read as a (bad) octal number.
        let rc_after = format!(
            "_SUVADU_BIN='{}'\n\
             zmodload -u zsh/datetime\n\
             typeset -g EPOCHREALTIME=1700000000.089123\n\
             {}",
            bin.display(),
            if when == "after" {
                "setopt OCTAL_ZEROES SH_WORD_SPLIT NO_UNSET"
            } else {
                ""
            }
        );
        let out = s.zsh(&zsh, options, &rc_after, "echo hi\nfalse\n");
        assert_clean(&out);
        let rows = s.recorded();
        let commands: Vec<&str> = rows.iter().map(|r| r.command.as_str()).collect();
        assert_eq!(
            commands,
            ["echo hi", "false"],
            "options set {when} the hook"
        );
        assert_eq!(rows[1].exit_code, Some(1), "options set {when} the hook");
        let out = s
            .suv()
            .args(["history", "--json", "-n", "1"])
            .output()
            .unwrap();
        let text = String::from_utf8_lossy(&out.stdout);
        assert!(
            text.contains("1700000000089"),
            "start time misread with options set {when} the hook: {text}"
        );
    }
}

// ── both shells: the managed aliases file ──────────────────

/// A HOME whose path needs quoting in every way a shell can get wrong.
const AWKWARD_HOME: &str = "it's \"q\" $HOME `echo x` dir";

#[test]
fn zsh_loads_aliases_from_a_path_that_needs_quoting() {
    let Some(zsh) = zsh_bin() else { return };
    let s = Sandbox::with_home_named(AWKWARD_HOME);
    let out = s
        .suv()
        .args(["aliases", "add", "hello", "echo hi-from-alias"])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let out = s.zsh(&zsh, "", "", "hello\n");
    assert_clean(&out);
    assert!(
        String::from_utf8_lossy(&out.stdout).contains("hi-from-alias"),
        "alias not loaded:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn bash_loads_aliases_from_a_path_that_needs_quoting() {
    let Some(bash) = modern_bash() else { return };
    let s = Sandbox::with_home_named(AWKWARD_HOME);
    let out = s
        .suv()
        .args(["aliases", "add", "hello", "echo hi-from-alias"])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let out = s.bash(&bash, "", "", "hello\n");
    assert_clean(&out);
    assert!(
        String::from_utf8_lossy(&out.stdout).contains("hi-from-alias"),
        "alias not loaded:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// Which of `names` a new shell has as aliases.
fn aliases_defined_in_new_shell(s: &Sandbox, shell: ShellKind, bin: &Path) -> Vec<String> {
    let probe =
        "for n in keep gone; do alias \"$n\" >/dev/null 2>&1 && echo \"defined:$n\"; done\n";
    let out = s.run_interactive(shell, bin, "", "", probe);
    assert_clean(&out);
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter_map(|l| l.strip_prefix("defined:").map(str::to_string))
        .collect()
}

#[test]
fn removed_aliases_are_gone_from_new_shells() {
    let shells: Vec<(ShellKind, PathBuf)> = [
        modern_bash().map(|b| (ShellKind::Bash, b)),
        zsh_bin().map(|z| (ShellKind::Zsh, z)),
    ]
    .into_iter()
    .flatten()
    .collect();
    for (kind, bin) in shells {
        let s = Sandbox::new();
        for (name, command) in [("keep", "echo kept"), ("gone", "echo gone")] {
            let out = s
                .suv()
                .args(["aliases", "add", name, command])
                .output()
                .unwrap();
            assert!(
                out.status.success(),
                "{}",
                String::from_utf8_lossy(&out.stderr)
            );
        }
        assert_eq!(
            aliases_defined_in_new_shell(&s, kind, &bin),
            ["keep", "gone"]
        );

        let out = s
            .suv()
            .args(["aliases", "remove", "gone"])
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert_eq!(
            aliases_defined_in_new_shell(&s, kind, &bin),
            ["keep"],
            "{} after removing one alias",
            kind.name()
        );

        // Removing the last alias must not leave the old file behind.
        let out = s
            .suv()
            .args(["aliases", "remove", "keep"])
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert!(
            aliases_defined_in_new_shell(&s, kind, &bin).is_empty(),
            "{} after removing every alias",
            kind.name()
        );
    }
}

// ── both shells: the git branch each command ran on ────────

/// Run `git` in `dir` with no user or system config, so the machine's own
/// git settings cannot change what the test sees.
fn git(s: &Sandbox, dir: &Path, args: &[&str]) -> String {
    let mut cmd = Command::new("git");
    s.isolate(&mut cmd);
    let out = cmd
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .args([
            "-c",
            "user.name=Test",
            "-c",
            "user.email=test@example.com",
            "-c",
            "commit.gpgsign=false",
        ])
        .args(args)
        .current_dir(dir)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).unwrap().trim().to_string()
}

fn git_available() -> bool {
    let found = Command::new("git")
        .arg("--version")
        .output()
        .is_ok_and(|o| o.status.success());
    if !found {
        eprintln!("skipping: git not found");
    }
    found
}

/// A sandbox holding a repository on `main` (`proj`, with `proj/sub/dir`),
/// a linked worktree of it on `topic` (`wt`), a repository with a detached
/// HEAD (`det`), one whose HEAD is garbage (`odd`) and a plain directory
/// (`plain`). Returns the detached HEAD's short hash.
fn branch_sandbox(s: &Sandbox) -> String {
    let proj = s.path("proj");
    std::fs::create_dir_all(proj.join("sub/dir")).unwrap();
    git(s, &proj, &["init", "-q", "-b", "main"]);
    git(s, &proj, &["commit", "-q", "--allow-empty", "-m", "first"]);
    git(s, &proj, &["worktree", "add", "-q", "-b", "topic", "../wt"]);

    let det = s.path("det");
    std::fs::create_dir_all(&det).unwrap();
    git(s, &det, &["init", "-q", "-b", "main"]);
    git(s, &det, &["commit", "-q", "--allow-empty", "-m", "first"]);
    git(s, &det, &["checkout", "-q", "--detach"]);
    let short = git(s, &det, &["rev-parse", "--short=7", "HEAD"]);

    std::fs::create_dir_all(s.path("odd/.git")).unwrap();
    std::fs::write(s.path("odd/.git/HEAD"), "this is not a ref\n").unwrap();
    std::fs::create_dir_all(s.path("plain")).unwrap();
    short
}

const BRANCH_TOUR: &str = "cd proj\n\
     echo in-root\n\
     cd sub/dir\n\
     echo in-nested\n\
     cd ../../../wt\n\
     echo in-worktree\n\
     cd ../det\n\
     echo in-detached\n\
     cd ../odd\n\
     echo in-odd\n\
     cd ../plain\n\
     echo in-plain\n";

#[test]
fn both_shells_record_the_branch_of_the_directory_each_command_started_in() {
    if !git_available() {
        return;
    }
    let shells: Vec<(ShellKind, PathBuf)> = [
        modern_bash().map(|b| (ShellKind::Bash, b)),
        zsh_bin().map(|z| (ShellKind::Zsh, z)),
    ]
    .into_iter()
    .flatten()
    .collect();
    for (kind, bin) in shells {
        let s = Sandbox::new();
        let short = branch_sandbox(&s);
        let out = s.run_interactive(kind, &bin, "", "", BRANCH_TOUR);
        assert_clean(&out);
        let rows = s.recorded();
        let got: Vec<(&str, Option<&str>)> = rows
            .iter()
            .map(|r| (r.command.as_str(), r.git_branch.as_deref()))
            .collect();
        // A `cd` is filed under the branch of the directory it left: the
        // one it was typed in.
        assert_eq!(
            got,
            [
                ("cd proj", None),
                ("echo in-root", Some("main")),
                ("cd sub/dir", Some("main")),
                ("echo in-nested", Some("main")),
                ("cd ../../../wt", Some("main")),
                ("echo in-worktree", Some("topic")),
                ("cd ../det", Some("topic")),
                ("echo in-detached", Some(short.as_str())),
                ("cd ../odd", Some(short.as_str())),
                ("echo in-odd", None),
                ("cd ../plain", None),
                ("echo in-plain", None),
            ],
            "{}",
            kind.name()
        );
        // The recorded directory is unchanged: where the shell ended up.
        assert!(rows[0].cwd.ends_with("/proj"), "{}", rows[0].cwd);
    }
}

#[test]
fn both_shells_record_no_branch_when_it_is_turned_off() {
    if !git_available() {
        return;
    }
    let shells: Vec<(ShellKind, PathBuf)> = [
        modern_bash().map(|b| (ShellKind::Bash, b)),
        zsh_bin().map(|z| (ShellKind::Zsh, z)),
    ]
    .into_iter()
    .flatten()
    .collect();
    for (kind, bin) in shells {
        let s = Sandbox::new();
        branch_sandbox(&s);
        let config = if cfg!(target_os = "macos") {
            s.path("Library/Application Support/tech.appachi.suvadu/config.toml")
        } else {
            s.path("config/suvadu/config.toml")
        };
        std::fs::create_dir_all(config.parent().unwrap()).unwrap();
        std::fs::write(&config, "[shell]\ncapture_git_branch = false\n").unwrap();
        // A project overlay cannot turn it back on.
        std::fs::write(
            s.path("proj/.suvadu.toml"),
            "[shell]\ncapture_git_branch = true\n",
        )
        .unwrap();
        let out = s.run_interactive(kind, &bin, "", "", "cd proj\necho in-root\n");
        assert_clean(&out);
        let rows = s.recorded();
        assert_eq!(rows.len(), 2, "{}: {rows:?}", kind.name());
        assert!(
            rows.iter().all(|r| r.git_branch.is_none()),
            "{}: {rows:?}",
            kind.name()
        );
    }
}

// ── zsh: the zsh-autosuggestions strategy ──────────────────

impl Sandbox {
    /// Record `command` as if a shell hook had, starting at `started_at`.
    fn seed(&self, command: &str, executor_type: &str, started_at: i64) {
        let out = self
            .suv()
            .args([
                "add",
                "--session-id",
                "seeded",
                "--command",
                command,
                "--cwd",
                &self.home.to_string_lossy(),
                "--exit-code",
                "0",
                "--started-at",
                &started_at.to_string(),
                "--ended-at",
                &(started_at + 5).to_string(),
                "--executor-type",
                executor_type,
                "--executor",
                if executor_type == "human" {
                    "terminal"
                } else {
                    "claude-code"
                },
            ])
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "seeding {command:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
}

/// Lines that call the strategy the way zsh-autosuggestions does, each
/// printing `<label>=[<suggestion>]`, plus whatever the call itself printed.
fn strategy_probe(label: &str, buffer: &str) -> String {
    // The buffer is single-quoted for the shell; none of the test buffers
    // contain a single quote.
    format!(
        "suggestion=stale; _zsh_autosuggest_strategy_suvadu '{buffer}'; \
         print -r -- \"{label}=[$suggestion]\"\n\
         print -r -- \"{label}-out=[$(_zsh_autosuggest_strategy_suvadu '{buffer}' 2>&1)]\"\n"
    )
}

fn probe_value<'a>(stdout: &'a str, label: &str) -> &'a str {
    let start = format!("{label}=[");
    let line = stdout
        .lines()
        .find(|l| l.starts_with(&start))
        .unwrap_or_else(|| panic!("no {label} line in:\n{stdout}"));
    &line[start.len()..line.len() - 1]
}

#[test]
fn zsh_autosuggest_strategy_suggests_the_newest_command_starting_with_the_buffer() {
    let Some(zsh) = zsh_bin() else { return };
    let s = Sandbox::new();
    let t = 1_700_000_000_000;
    s.seed("git checkout main", "human", t);
    s.seed("git cherry-pick abc123", "human", t + 1000);
    s.seed("git checkout older", "human", t + 2000);
    s.seed("git checkout from-an-agent", "agent", t + 3000);
    s.seed("--version --verbose", "human", t + 4000);
    s.seed("GIT STATUS", "human", t + 5000);
    s.seed("--help me", "human", t + 6000);

    let lines = [
        strategy_probe("checkout", "git ch"),
        strategy_probe("empty", ""),
        strategy_probe("nomatch", "zz-no-such-command"),
        strategy_probe("hyphen", "--vers"),
        strategy_probe("help", "--help"),
        strategy_probe("case", "git st"),
        // Alt+A's per-shell switch shows agent commands, as in Up-arrow.
        "_SUVADU_SHOW_AGENTS=1\n".to_string(),
        strategy_probe("agents", "git ch"),
        "unset _SUVADU_SHOW_AGENTS\n".to_string(),
        // Options a user may have set must not change the result.
        "setopt SH_WORD_SPLIT KSH_ARRAYS NO_UNSET GLOB_SUBST\n".to_string(),
        strategy_probe("options", "git ch"),
        "unsetopt SH_WORD_SPLIT KSH_ARRAYS NO_UNSET GLOB_SUBST\n".to_string(),
        // No binary anywhere: nothing suggested, nothing printed.
        "PATH=/usr/bin:/bin; _SUVADU_BIN=/no/such/suv\n".to_string(),
        strategy_probe("missing", "git ch"),
    ]
    .concat();
    let out = s.zsh(&zsh, "", "", &lines);
    assert_clean(&out);
    let stdout = String::from_utf8_lossy(&out.stdout);

    assert_eq!(probe_value(&stdout, "checkout"), "git checkout older");
    assert_eq!(probe_value(&stdout, "empty"), "");
    assert_eq!(probe_value(&stdout, "nomatch"), "");
    assert_eq!(probe_value(&stdout, "hyphen"), "--version --verbose");
    assert_eq!(probe_value(&stdout, "help"), "--help me");
    // History search ignores ASCII case; a suggestion must extend what
    // was typed exactly, so a differently-cased match is not offered.
    assert_eq!(probe_value(&stdout, "case"), "");
    assert_eq!(probe_value(&stdout, "agents"), "git checkout from-an-agent");
    assert_eq!(probe_value(&stdout, "options"), "git checkout older");
    assert_eq!(probe_value(&stdout, "missing"), "");
    for label in [
        "checkout", "empty", "nomatch", "hyphen", "help", "case", "agents", "options", "missing",
    ] {
        assert_eq!(
            probe_value(&stdout, &format!("{label}-out")),
            "",
            "the strategy printed something for {label}"
        );
    }
}

#[test]
fn zsh_init_explains_how_to_enable_the_autosuggest_strategy() {
    let s = Sandbox::new();
    let out = s.suv().args(["init", "zsh"]).output().unwrap();
    let script = String::from_utf8_lossy(&out.stdout);
    assert!(
        script.contains("ZSH_AUTOSUGGEST_STRATEGY=(suvadu history)"),
        "{script}"
    );
    assert!(
        !script
            .lines()
            .any(|l| l.trim_start().starts_with("ZSH_AUTOSUGGEST_STRATEGY")),
        "the hook must not change the user's strategy itself"
    );
}
