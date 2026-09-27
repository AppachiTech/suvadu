//! The "new version available" notice: when it appears, and when it never
//! does.
//!
//! Every run uses a private HOME whose saved update state says a check just
//! happened, so no test starts a background check or touches the network.

#![cfg(unix)]
// A real terminal is the only way to see what a person at one would see;
// opening a pty needs libc.
#![allow(unsafe_code)]

use std::io::Read;
use std::os::unix::io::FromRawFd;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const FUTURE: &str = "99.0.0";

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs()
}

/// Both places the data directory can be, whichever this platform uses.
fn state_paths(home: &Path) -> [PathBuf; 2] {
    [
        home.join("Library/Application Support/tech.appachi.suvadu/update-check.json"),
        home.join("data/suvadu/update-check.json"),
    ]
}

fn seed(home: &Path, latest: &str) {
    let state = format!(
        r#"{{"checked_at":{},"latest":"{latest}","notified_at":null}}"#,
        now()
    );
    for path in state_paths(home) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, &state).unwrap();
    }
}

fn states(home: &Path) -> Vec<String> {
    state_paths(home)
        .iter()
        .map(|p| std::fs::read_to_string(p).unwrap_or_default())
        .collect()
}

fn command(home: &Path, args: &[&str], env: &[(&str, &str)]) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_suv"));
    command
        .args(args)
        .env_clear()
        .env("HOME", home)
        .env("XDG_DATA_HOME", home.join("data"))
        .env("XDG_CONFIG_HOME", home.join("config"))
        .env("PATH", "/usr/bin:/bin")
        .env("TERM", "xterm-256color")
        .envs(env.iter().copied())
        .current_dir(home);
    command
}

/// Run with stdout and stderr on a terminal, as a person would, and return
/// everything written to it.
fn at_terminal(home: &Path, args: &[&str], env: &[(&str, &str)]) -> String {
    let (mut master, mut slave) = (-1, -1);
    // SAFETY: out-parameters we own; no name, termios or size is needed.
    let rc = unsafe {
        libc::openpty(
            &raw mut master,
            &raw mut slave,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
        )
    };
    assert_eq!(rc, 0);
    // SAFETY: `master` is ours; the child must not inherit it.
    unsafe {
        libc::fcntl(master, libc::F_SETFD, libc::FD_CLOEXEC);
        libc::fcntl(master, libc::F_SETFL, libc::O_NONBLOCK);
    }
    let mut child = command(home, args, env)
        .stdin(Stdio::null())
        // SAFETY: each dup is handed to the child as a standard stream.
        .stdout(unsafe { Stdio::from_raw_fd(libc::dup(slave)) })
        .stderr(unsafe { Stdio::from_raw_fd(libc::dup(slave)) })
        .spawn()
        .unwrap();
    // SAFETY: the child holds its own copies.
    unsafe { libc::close(slave) };
    // SAFETY: the master fd is ours alone and closed when this drops.
    let mut terminal = unsafe { std::fs::File::from_raw_fd(master) };
    let mut seen = Vec::new();
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        let mut chunk = [0u8; 4096];
        match terminal.read(&mut chunk) {
            Ok(read) if read > 0 => seen.extend_from_slice(&chunk[..read]),
            _ => std::thread::sleep(Duration::from_millis(5)),
        }
        if child.try_wait().unwrap().is_some() {
            // Collect whatever is still buffered.
            while let Ok(read) = terminal.read(&mut chunk) {
                if read == 0 {
                    break;
                }
                seen.extend_from_slice(&chunk[..read]);
            }
            break;
        }
        assert!(Instant::now() < deadline, "suv {args:?} did not finish");
    }
    String::from_utf8_lossy(&seen).into_owned()
}

#[test]
fn a_newer_release_is_mentioned_once_a_day_at_a_terminal() {
    let home = tempfile::tempdir().unwrap();
    seed(home.path(), FUTURE);
    let first = at_terminal(home.path(), &["version"], &[]);
    let expected = format!(
        "suvadu {FUTURE} is available (you have {}) — update with: suv update",
        env!("CARGO_PKG_VERSION")
    );
    assert!(first.contains(&expected), "{first}");
    assert!(first.contains(&format!("suvadu v{}", env!("CARGO_PKG_VERSION"))));

    let second = at_terminal(home.path(), &["version"], &[]);
    assert!(
        !second.contains("is available"),
        "shown twice in a day: {second}"
    );
}

#[test]
fn nothing_is_said_or_checked_in_a_pipe_in_ci_or_when_turned_off() {
    let home = tempfile::tempdir().unwrap();
    seed(home.path(), FUTURE);
    let before = states(home.path());

    let piped = command(home.path(), &["version"], &[]).output().unwrap();
    assert!(!String::from_utf8_lossy(&piped.stderr).contains("is available"));

    for env in [
        &[("SUVADU_NO_UPDATE_CHECK", "1")][..],
        &[("CI", "true")][..],
    ] {
        let shown = at_terminal(home.path(), &["version"], env);
        assert!(!shown.contains("is available"), "{env:?}: {shown}");
    }

    for path in [
        home.path()
            .join("Library/Application Support/tech.appachi.suvadu/config.toml"),
        home.path().join("config/suvadu/config.toml"),
    ] {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, "[update]\ncheck = false\n").unwrap();
    }
    let shown = at_terminal(home.path(), &["version"], &[]);
    assert!(!shown.contains("is available"), "config off: {shown}");

    assert_eq!(states(home.path()), before, "the saved state changed");
}

/// Recall runs inside the shell's Ctrl+R widget, and `suv pause` prints
/// shell code to eval: neither may say anything extra.
#[test]
fn recall_and_pause_never_announce() {
    let home = tempfile::tempdir().unwrap();
    seed(home.path(), FUTURE);
    for args in [&["search"][..], &["pause"][..]] {
        let shown = at_terminal(home.path(), args, &[]);
        assert!(!shown.contains("is available"), "{args:?}: {shown}");
    }
}
