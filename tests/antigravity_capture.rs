//! Antigravity agent capture, end to end: `suv init antigravity` against a
//! private HOME, then commands run the way Antigravity's agent runs them —
//! `zsh -c '<command>'`, non-interactive, with `ANTIGRAVITY_AGENT` set — and
//! read back with `suv history --json`. The user's own `~/.zshenv`, config
//! and history are never touched.
#![cfg(unix)]

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

/// One recorded history row.
#[derive(Debug)]
struct Recorded {
    command: String,
    cwd: String,
    exit_code: Option<i64>,
    executor_type: Option<String>,
    executor: Option<String>,
    session_id: String,
}

struct Sandbox {
    _root: tempfile::TempDir,
    home: PathBuf,
    zsh: Option<PathBuf>,
}

impl Sandbox {
    fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        let home = root.path().join("home");
        std::fs::create_dir_all(&home).unwrap();
        Self {
            _root: root,
            home,
            zsh: zsh_bin(),
        }
    }

    /// A sandbox for a test that runs agent commands, or `None` (and the
    /// test is skipped) on a machine without zsh.
    fn with_zsh() -> Option<Self> {
        let sb = Self::new();
        if sb.zsh.is_none() {
            eprintln!("skipping: zsh not found");
            return None;
        }
        Some(sb)
    }

    fn path(&self, name: &str) -> PathBuf {
        self.home.join(name)
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

    fn suv(&self) -> Command {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_suv"));
        self.isolate(&mut cmd);
        cmd
    }

    fn init(&self) -> Output {
        let out = self.suv().args(["init", "antigravity"]).output().unwrap();
        assert!(
            out.status.success(),
            "suv init antigravity failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        out
    }

    fn zshenv(&self) -> String {
        std::fs::read_to_string(self.path(".zshenv")).unwrap_or_default()
    }

    /// Run `command` as Antigravity's agent does. `agent` controls whether
    /// `ANTIGRAVITY_AGENT` is set. -d skips /etc/zsh*, so only the sandbox
    /// `~/.zshenv` runs.
    fn agent_zsh(&self, command: &str, agent: bool, cwd: &Path) -> Output {
        let mut cmd = Command::new(self.zsh.as_ref().expect("Sandbox::with_zsh"));
        self.isolate(&mut cmd);
        if agent {
            cmd.env("ANTIGRAVITY_AGENT", "1");
        }
        cmd.arg("-d").arg("-c").arg(command).current_dir(cwd);
        cmd.output().unwrap()
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
                (
                    v["id"].as_i64().unwrap(),
                    Recorded {
                        command: v["command"].as_str().unwrap().to_string(),
                        cwd: v["cwd"].as_str().unwrap().to_string(),
                        exit_code: v["exit_code"].as_i64(),
                        executor_type: v["executor_type"].as_str().map(String::from),
                        executor: v["executor"].as_str().map(String::from),
                        session_id: v["session_id"].as_str().unwrap().to_string(),
                    },
                )
            })
            .collect();
        rows.sort_by_key(|(id, _)| *id);
        rows.into_iter().map(|(_, r)| r).collect()
    }
}

fn zsh_bin() -> Option<PathBuf> {
    [
        "/bin/zsh",
        "/usr/bin/zsh",
        "/opt/homebrew/bin/zsh",
        "/usr/local/bin/zsh",
    ]
    .iter()
    .map(PathBuf::from)
    .find(|p| p.exists())
}

fn canonical(path: &Path) -> String {
    std::fs::canonicalize(path)
        .unwrap()
        .to_string_lossy()
        .to_string()
}

#[test]
fn an_agent_command_is_recorded_with_its_exit_code_and_directory() {
    let Some(sb) = Sandbox::with_zsh() else {
        return;
    };
    sb.init();
    let project = sb.path("project");
    std::fs::create_dir_all(project.join("sub")).unwrap();

    // The last command is external, so zsh would normally exec it in place
    // of the shell; it must still be recorded, and its status kept.
    let out = sb.agent_zsh("cd sub && ls /suvadu-does-not-exist", true, &project);
    assert_eq!(out.status.code(), Some(1), "the command's own exit status");

    let rows = sb.recorded();
    assert_eq!(rows.len(), 1, "{rows:?}");
    let row = &rows[0];
    assert_eq!(row.command, "cd sub && ls /suvadu-does-not-exist");
    assert_eq!(row.exit_code, Some(1));
    assert_eq!(row.cwd, canonical(&project.join("sub")));
    assert_eq!(row.executor_type.as_deref(), Some("agent"));
    assert_eq!(row.executor.as_deref(), Some("antigravity"));
    assert!(
        row.session_id.starts_with("antigravity-"),
        "{}",
        row.session_id
    );
}

#[test]
fn shells_outside_antigravity_are_left_alone() {
    let Some(sb) = Sandbox::with_zsh() else {
        return;
    };
    sb.init();
    let out = sb.agent_zsh("true", false, &sb.home);
    assert!(out.status.success());
    assert!(sb.recorded().is_empty());
}

#[test]
fn a_nested_zsh_inside_an_agent_command_is_not_recorded_twice() {
    let Some(sb) = Sandbox::with_zsh() else {
        return;
    };
    sb.init();
    let out = sb.agent_zsh("zsh -d -c 'echo inner'", true, &sb.home);
    assert!(out.status.success());
    let commands: Vec<String> = sb.recorded().into_iter().map(|r| r.command).collect();
    assert_eq!(commands, vec!["zsh -d -c 'echo inner'".to_string()]);
}

#[test]
fn a_command_that_sets_its_own_exit_trap_is_still_recorded() {
    let Some(sb) = Sandbox::with_zsh() else {
        return;
    };
    sb.init();
    let out = sb.agent_zsh("trap 'echo mine' EXIT; exit 3", true, &sb.home);
    assert_eq!(out.status.code(), Some(3));
    assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "mine");
    let rows = sb.recorded();
    assert_eq!(rows.len(), 1, "{rows:?}");
    assert_eq!(rows[0].exit_code, Some(3));
}

#[test]
fn the_agent_command_prints_nothing_extra() {
    let Some(sb) = Sandbox::with_zsh() else {
        return;
    };
    sb.init();
    let out = sb.agent_zsh("echo hello", true, &sb.home);
    assert_eq!(String::from_utf8_lossy(&out.stdout), "hello\n");
    assert_eq!(String::from_utf8_lossy(&out.stderr), "");
}

#[test]
fn init_keeps_the_existing_zshenv_backs_it_up_and_is_idempotent() {
    let sb = Sandbox::new();
    let original = "export EDITOR=vim\n";
    std::fs::write(sb.path(".zshenv"), original).unwrap();

    sb.init();
    sb.init();

    let zshenv = sb.zshenv();
    assert!(zshenv.starts_with(original), "{zshenv}");
    assert_eq!(
        zshenv.matches("# >>> suvadu antigravity >>>").count(),
        1,
        "{zshenv}"
    );
    let backups: Vec<PathBuf> = std::fs::read_dir(&sb.home)
        .unwrap()
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            p.file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with(".zshenv.suvadu-backup-")
        })
        .collect();
    assert_eq!(backups.len(), 1, "only the first init changed the file");
    assert_eq!(std::fs::read_to_string(&backups[0]).unwrap(), original);
}

#[test]
fn init_creates_a_missing_zshenv_without_a_backup() {
    let sb = Sandbox::new();
    let out = sb.init();
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains(".zshenv"), "{stdout}");
    assert!(sb.zshenv().contains("# >>> suvadu antigravity >>>"));
    let has_backup = std::fs::read_dir(&sb.home)
        .unwrap()
        .flatten()
        .any(|e| e.file_name().to_string_lossy().contains("suvadu-backup"));
    assert!(!has_backup);
}

#[test]
fn doctor_reports_the_integration_as_installed() {
    let sb = Sandbox::new();
    // The Antigravity check's line and its aspects line beneath it.
    let antigravity = || -> String {
        let out = sb.suv().arg("doctor").output().unwrap();
        let text = String::from_utf8_lossy(&out.stdout).to_string();
        let lines: Vec<&str> = text.lines().collect();
        let at = lines
            .iter()
            .position(|l| l.trim_start().starts_with("Antigravity "))
            .unwrap_or_else(|| panic!("no Antigravity check in:\n{text}"));
        lines[at..(at + 2).min(lines.len())].join("\n")
    };
    let before = antigravity();
    assert!(before.contains("not in use"), "{before}");
    sb.init();
    let after = antigravity();
    assert!(after.contains("integration: installed"), "{after}");

    // Losing the ~/.zshenv block breaks capture even with the script there.
    std::fs::write(sb.path(".zshenv"), "export EDITOR=vim\n").unwrap();
    let broken = antigravity();
    assert!(broken.contains("installed but broken"), "{broken}");
    assert!(broken.contains(".zshenv"), "{broken}");
}
