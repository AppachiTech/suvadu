//! The generated shell hooks, running inside real interactive shells.
//!
//! Each test writes the hook that `suv init <shell>` prints into a private
//! HOME, starts an interactive shell whose rc file sources it, and types
//! lines into the shell's stdin. Whatever the hook records goes into that
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
    exit_code: Option<i64>,
    duration_ms: i64,
}

struct Sandbox {
    home: tempfile::TempDir,
}

impl Sandbox {
    fn new() -> Self {
        Self {
            home: tempfile::tempdir().unwrap(),
        }
    }

    fn path(&self, name: &str) -> PathBuf {
        self.home.path().join(name)
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
            .env("HOME", self.home.path())
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
        }
        cmd.current_dir(self.home.path())
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
}

impl ShellKind {
    const fn name(self) -> &'static str {
        match self {
            Self::Bash => "bash",
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
