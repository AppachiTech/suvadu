//! `suv add`, the command every shell and agent hook runs once per
//! recorded command. Each test runs the real binary against a private HOME
//! with its own database; the user's own data is never opened.
use std::path::PathBuf;
use std::process::{Command, Output};
use std::sync::{Arc, Barrier};

struct Sandbox {
    home: tempfile::TempDir,
}

impl Sandbox {
    fn new() -> Self {
        Self {
            home: tempfile::tempdir().unwrap(),
        }
    }

    fn home(&self) -> PathBuf {
        self.home.path().to_path_buf()
    }

    fn command(home: &std::path::Path, args: &[&str]) -> Command {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_suv"));
        cmd.args(args)
            .env("HOME", home)
            .env("XDG_DATA_HOME", home.join("data"))
            .env("XDG_CONFIG_HOME", home.join("config"))
            .env_remove("SUVADU_PAUSED")
            .env_remove("SUVADU_SESSION_ID")
            .env("NO_COLOR", "1");
        cmd
    }

    fn run(&self, args: &[&str]) -> Output {
        Self::command(self.home.path(), args).output().unwrap()
    }

    fn add(home: &std::path::Path, session: &str, command: &str) -> Output {
        Self::command(
            home,
            &[
                "add",
                "--session-id",
                session,
                "--command",
                command,
                "--cwd",
                "/work",
                "--exit-code",
                "0",
                "--started-at",
                "1700000000000",
                "--ended-at",
                "1700000000100",
            ],
        )
        .output()
        .unwrap()
    }

    fn commands(&self) -> Vec<String> {
        let out = self.run(&["history", "--json", "-n", "1000"]);
        assert!(
            out.status.success(),
            "suv history failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        let mut commands: Vec<String> = String::from_utf8(out.stdout)
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
            .map(|v| v["command"].as_str().unwrap().to_string())
            .collect();
        commands.sort();
        commands
    }
}

/// An agent running several commands at once starts a session with all of
/// them: every one must be recorded, however the session row's creation
/// interleaves.
#[test]
fn parallel_first_commands_of_one_session_are_all_recorded() {
    const SESSIONS: usize = 4;
    const PARALLEL: usize = 12;
    let s = Sandbox::new();
    // Create the database up front so only session creation races.
    let out = Sandbox::add(&s.home(), "warm-up", "true");
    assert!(out.status.success());

    let mut expected = vec!["true".to_string()];
    for round in 0..SESSIONS {
        let barrier = Arc::new(Barrier::new(PARALLEL));
        let session = format!("agent-session-{round}");
        let handles: Vec<_> = (0..PARALLEL)
            .map(|i| {
                let barrier = Arc::clone(&barrier);
                let home = s.home();
                let session = session.clone();
                std::thread::spawn(move || {
                    barrier.wait();
                    Sandbox::add(&home, &session, &format!("echo {round}-{i}"))
                })
            })
            .collect();
        for h in handles {
            let out = h.join().unwrap();
            assert!(
                out.status.success(),
                "suv add failed: {}",
                String::from_utf8_lossy(&out.stderr)
            );
        }
        expected.extend((0..PARALLEL).map(|i| format!("echo {round}-{i}")));
    }
    expected.sort();
    assert_eq!(s.commands(), expected);
}

/// A typed line that starts with a dash is recorded like any other.
#[test]
fn commands_that_start_with_a_dash_are_recorded() {
    let s = Sandbox::new();
    for typed in ["-n foo", "--x", "-"] {
        let out = Sandbox::add(&s.home(), "dash-session", typed);
        assert!(
            out.status.success(),
            "suv add --command {typed:?} failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
    assert_eq!(s.commands(), ["-", "--x", "-n foo"]);
}
