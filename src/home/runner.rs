//! Running a feature Home opens: the current executable with the catalog's
//! fixed arguments, never a shell, and what came back turned into something
//! to show.
//!
//! Three shapes, by launch mode:
//! - Interactive: the child gets the terminal and keeps it until it exits.
//! - Selection: the child draws on the terminal; its stdout — the picked
//!   command — is captured, drained as it arrives and capped at 1 MiB.
//! - Report: no terminal at all; stdout and stderr are drained side by side,
//!   each capped at 1 MiB, and after five seconds the child and everything
//!   it started are stopped.

use std::io::Read;
use std::path::Path;
use std::process::{Command, ExitStatus, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use super::catalog::{self, LaunchMode, LaunchRequest};
use super::model::Outcome;

/// Most output kept from one stream. The rest is still read, so a child
/// can never block on a full pipe, and the result says it was cut.
pub const CAPTURE_LIMIT: usize = 1024 * 1024;

/// Set for every feature Home opens, so a picker can explain itself
/// through its exit status (see `EXIT_NO_HISTORY`) instead of text Home's
/// screen would cover. The shell's own widgets never set it.
pub const LAUNCHED_BY_HOME: &str = "SUVADU_LAUNCHED_BY_HOME";

/// How long a report may take before it is stopped.
pub const REPORT_TIMEOUT: Duration = Duration::from_secs(5);

/// After the child is gone, how long to wait for its streams to close. A
/// process it started may still hold them; its output is not waited for.
const DRAIN_GRACE: Duration = Duration::from_secs(1);

#[derive(Debug, Default)]
pub struct LaunchResult {
    /// `None` when the child was ended by a signal.
    pub code: Option<i32>,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
    /// Some output went past [`CAPTURE_LIMIT`] and was not kept.
    pub truncated: bool,
    pub timed_out: bool,
    /// How long the child ran.
    pub elapsed: Duration,
}

pub fn run_child(executable: &Path, request: &LaunchRequest) -> std::io::Result<LaunchResult> {
    run_with(executable, request, REPORT_TIMEOUT)
}

/// Run `executable` with the request's arguments, as separate arguments and
/// without a shell, in the shape its mode needs.
pub fn run_with(
    executable: &Path,
    request: &LaunchRequest,
    report_timeout: Duration,
) -> std::io::Result<LaunchResult> {
    let started = Instant::now();
    let mut command = Command::new(executable);
    // Home shows a known update itself: the feature it opens must neither
    // print the notice over its own screen nor start a check.
    command
        .args(&request.args)
        .env(crate::update_check::OPT_OUT_ENV, "1")
        .env(LAUNCHED_BY_HOME, "1");
    match request.mode {
        LaunchMode::Interactive => {
            let status = command.status()?;
            return Ok(LaunchResult {
                code: status.code(),
                elapsed: started.elapsed(),
                ..LaunchResult::default()
            });
        }
        LaunchMode::Selection => {
            command.stdout(Stdio::piped());
        }
        LaunchMode::Report => {
            command
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped());
            // Its own process group, so a timeout can stop everything it
            // started. A report never uses the terminal, so leaving the
            // foreground group costs nothing; screens and pickers stay in it.
            #[cfg(unix)]
            {
                use std::os::unix::process::CommandExt;
                command.process_group(0);
            }
        }
    }
    let mut child = command.spawn()?;
    let stdout = child.stdout.take().map(drain);
    let stderr = child.stderr.take().map(drain);
    let (status, timed_out) = if request.mode == LaunchMode::Report {
        wait_with_deadline(&mut child, report_timeout)?
    } else {
        (child.wait()?, false)
    };
    let elapsed = started.elapsed();
    let collect = |rx: Option<mpsc::Receiver<(Vec<u8>, bool)>>| {
        rx.and_then(|rx| rx.recv_timeout(DRAIN_GRACE).ok())
            .unwrap_or_default()
    };
    let (stdout, out_cut) = collect(stdout);
    let (stderr, err_cut) = collect(stderr);
    Ok(LaunchResult {
        code: status.code(),
        stdout,
        stderr,
        truncated: out_cut || err_cut,
        timed_out,
        elapsed,
    })
}

/// Read a stream to its end on its own thread, keeping at most
/// [`CAPTURE_LIMIT`] bytes and reporting whether more arrived.
fn drain<R: Read + Send + 'static>(mut stream: R) -> mpsc::Receiver<(Vec<u8>, bool)> {
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let mut kept = Vec::new();
        let mut truncated = false;
        let mut buffer = [0u8; 16 * 1024];
        loop {
            match stream.read(&mut buffer) {
                Ok(0) => break,
                Ok(read) => {
                    let room = CAPTURE_LIMIT - kept.len();
                    truncated |= read > room;
                    kept.extend_from_slice(&buffer[..read.min(room)]);
                }
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
                Err(_) => break,
            }
        }
        let _ = tx.send((kept, truncated));
    });
    rx
}

/// Wait for the child, stopping and reaping it at the deadline.
fn wait_with_deadline(
    child: &mut std::process::Child,
    timeout: Duration,
) -> std::io::Result<(ExitStatus, bool)> {
    let deadline = Instant::now() + timeout;
    loop {
        if let Some(status) = child.try_wait()? {
            return Ok((status, false));
        }
        if Instant::now() >= deadline {
            kill_group(child);
            let _ = child.kill();
            return Ok((child.wait()?, true));
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// Stop a report child and every process it started: it leads its own
/// process group, so the whole group is signalled.
#[cfg(unix)]
#[allow(unsafe_code)]
fn kill_group(child: &std::process::Child) {
    if let Ok(pid) = libc::pid_t::try_from(child.id()) {
        // SAFETY: killpg only sends a signal. `pid` is the group this child
        // leads (it was started with process_group(0)) and has not been
        // reaped yet, so the group cannot belong to anything else.
        unsafe {
            libc::killpg(pid, libc::SIGKILL);
        }
    }
}

#[cfg(not(unix))]
fn kill_group(_child: &std::process::Child) {}

/// What to show for a finished launch; `None` when a screen simply closed.
pub fn outcome(request: &LaunchRequest, result: &LaunchResult) -> Option<Outcome> {
    let command = catalog::feature(request.feature).map_or("suv", |f| f.command);
    let failed = |message: String, retry: bool| Some(Outcome::Failed { message, retry });
    let ended = || {
        result.code.map_or_else(
            || format!("{command} was stopped by a signal."),
            |code| format!("{command} exited with status {code}."),
        )
    };
    match request.mode {
        LaunchMode::Interactive if result.code == Some(0) => None,
        LaunchMode::Interactive => failed(ended(), false),
        LaunchMode::Report if result.timed_out => failed(
            format!(
                "{command} did not finish within {} seconds, so it was stopped.",
                REPORT_TIMEOUT.as_secs()
            ),
            true,
        ),
        LaunchMode::Report => Some(Outcome::Report {
            output: String::from_utf8_lossy(&result.stdout).into_owned(),
            errors: String::from_utf8_lossy(&result.stderr).into_owned(),
            code: result.code,
            truncated: result.truncated,
        }),
        LaunchMode::Selection => match result.code {
            Some(0) => selection(command, result),
            Some(crate::commands::search::EXIT_NO_HISTORY) => failed(
                "No commands are recorded yet, so there is nothing to search. Set up Shell \
                 integration (under Connect tools), open a new terminal and run a few commands."
                    .to_string(),
                false,
            ),
            Some(10) => failed(
                "Search is unavailable because recording is off or paused in this shell. \
                 suv enable turns recording on; a pause lasts until SUVADU_PAUSED is unset \
                 in the shell Home was started from."
                    .to_string(),
                false,
            ),
            _ => failed(ended(), false),
        },
    }
}

/// A picker prints its choice followed by one newline. Only that newline is
/// removed; anything cut short, too large or not text is refused rather
/// than offered as a command.
fn selection(command: &str, result: &LaunchResult) -> Option<Outcome> {
    let refuse = |message: String| {
        Some(Outcome::Failed {
            message,
            retry: false,
        })
    };
    if result.truncated {
        return refuse(format!(
            "The command {command} returned is too large to copy safely (over 1 MiB), so it \
             was not kept."
        ));
    }
    if result.stdout.is_empty() {
        return Some(Outcome::NothingSelected);
    }
    let Some(body) = result.stdout.strip_suffix(b"\n") else {
        return refuse(format!(
            "{command} did not finish its answer, so nothing was kept."
        ));
    };
    String::from_utf8(body.to_vec()).map_or_else(
        |_| {
            refuse(format!(
                "{command} returned something that is not text, so it was not kept."
            ))
        },
        |text| Some(Outcome::Selected(text)),
    )
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::ffi::OsString;
    use std::os::unix::fs::PermissionsExt;
    use std::path::PathBuf;

    use super::super::catalog::{FeatureId, LaunchMode, LaunchRequest};
    use super::super::model::Outcome;

    /// An executable shell script in its own temporary directory.
    fn fixture(dir: &tempfile::TempDir, body: &str) -> PathBuf {
        let path = dir.path().join("fixture");
        std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        path
    }

    fn request(feature: &'static str, args: &[&str], mode: LaunchMode) -> LaunchRequest {
        LaunchRequest {
            feature: FeatureId(feature),
            args: args.iter().map(OsString::from).collect(),
            mode,
        }
    }

    fn run(body: &str, args: &[&str], mode: LaunchMode) -> (LaunchResult, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let exe = fixture(&dir, body);
        let result =
            run_with(&exe, &request("status", args, mode), Duration::from_secs(5)).unwrap();
        (result, dir)
    }

    fn selection(body: &str) -> Option<Outcome> {
        let (result, _dir) = run(body, &[], LaunchMode::Selection);
        outcome(
            &request("search", &["search"], LaunchMode::Selection),
            &result,
        )
    }

    #[test]
    fn arguments_arrive_exactly_and_are_never_run_by_a_shell() {
        let dir = tempfile::tempdir().unwrap();
        let sentinel = dir.path().join("sentinel");
        let exe = fixture(&dir, r#"for a in "$@"; do printf '%s\0' "$a"; done"#);
        let args = [
            "plain".to_string(),
            "two words".to_string(),
            "quote\"s and 'single'".to_string(),
            format!("semi; touch {}", sentinel.display()),
            "$HOME `id` $(id)".to_string(),
            "தமிழ் 日本語 e\u{301}".to_string(),
            "*".to_string(),
        ];
        let arg_refs: Vec<&str> = args.iter().map(String::as_str).collect();
        let result = run_with(
            &exe,
            &request("status", &arg_refs, LaunchMode::Report),
            Duration::from_secs(5),
        )
        .unwrap();
        let received: Vec<String> = result
            .stdout
            .split(|b| *b == 0)
            .filter(|a| !a.is_empty())
            .map(|a| String::from_utf8(a.to_vec()).unwrap())
            .collect();
        assert_eq!(received, args);
        assert!(!sentinel.exists(), "an argument was run as a command");
    }

    #[test]
    fn both_streams_drain_past_a_pipe_buffer_without_deadlock() {
        let started = std::time::Instant::now();
        let (result, _dir) = run(
            "head -c 300000 /dev/zero | tr '\\0' a; head -c 300000 /dev/zero | tr '\\0' b >&2",
            &[],
            LaunchMode::Report,
        );
        assert_eq!(result.code, Some(0));
        assert_eq!(result.stdout.len(), 300_000);
        assert_eq!(result.stderr.len(), 300_000);
        assert!(!result.truncated && !result.timed_out);
        assert!(started.elapsed() < Duration::from_secs(10));
    }

    #[test]
    fn output_past_the_limit_is_drained_kept_to_the_limit_and_marked() {
        let (result, _dir) = run(
            "head -c 1600000 /dev/zero | tr '\\0' a",
            &[],
            LaunchMode::Report,
        );
        assert_eq!(result.code, Some(0), "the child was never blocked");
        assert_eq!(result.stdout.len(), CAPTURE_LIMIT);
        assert!(result.truncated);
    }

    #[test]
    fn a_selection_keeps_every_space_and_loses_only_the_final_newline() {
        assert_eq!(
            selection(r#"printf '%s\n' "  printf 'a\\nb'  ""#),
            Some(Outcome::Selected("  printf 'a\\nb'  ".into()))
        );
        assert_eq!(
            selection(r"printf 'first\nsecond\n\n'"),
            Some(Outcome::Selected("first\nsecond\n".into()))
        );
    }

    #[test]
    fn cancelling_a_picker_selects_nothing() {
        assert_eq!(selection("exit 0"), Some(Outcome::NothingSelected));
    }

    #[test]
    fn exit_10_explains_that_recording_is_off() {
        let Some(Outcome::Failed { message, retry }) = selection("exit 10") else {
            panic!()
        };
        assert!(
            message.contains("recording") && message.contains("paused"),
            "{message}"
        );
        assert!(!retry);
    }

    #[test]
    fn other_exits_and_signals_are_reported_plainly() {
        let Some(Outcome::Failed { message, .. }) = selection("exit 2") else {
            panic!()
        };
        assert!(message.contains("exited with status 2"), "{message}");
        let Some(Outcome::Failed { message, .. }) = selection("kill -TERM $$") else {
            panic!()
        };
        assert!(message.contains("signal"), "{message}");
    }

    #[test]
    fn an_oversized_or_unfinished_selection_is_never_offered() {
        let Some(Outcome::Failed { message, .. }) =
            selection("head -c 1100000 /dev/zero | tr '\\0' a; echo")
        else {
            panic!()
        };
        assert!(message.contains("too large"), "{message}");
        let Some(Outcome::Failed { message, .. }) = selection("printf 'no newline'") else {
            panic!()
        };
        assert!(message.contains("did not finish"), "{message}");
        let Some(Outcome::Failed { .. }) = selection(r"printf '\377\n'") else {
            panic!("invalid UTF-8 must not become a command")
        };
    }

    #[test]
    fn a_slow_report_is_stopped_reaped_and_offered_again() {
        let dir = tempfile::tempdir().unwrap();
        let exe = fixture(&dir, "echo started; exec sleep 30");
        let started = std::time::Instant::now();
        let request = request("status", &[], LaunchMode::Report);
        let result = run_with(&exe, &request, Duration::from_millis(300)).unwrap();
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "{:?}",
            started.elapsed()
        );
        assert!(result.timed_out);
        // Whatever it printed before the deadline is kept exactly. (Under
        // load the first run of a new script can miss the deadline before
        // printing anything, so its absence is not a failure.)
        assert!(
            result.stdout.is_empty() || result.stdout == b"started\n",
            "{:?}",
            String::from_utf8_lossy(&result.stdout)
        );
        let Some(Outcome::Failed { message, retry }) = outcome(&request, &result) else {
            panic!()
        };
        assert!(retry, "{message}");
        assert!(
            message.contains("5 seconds") || message.contains("stopped"),
            "{message}"
        );
    }

    /// A report that hangs in a process it started (doctor's shell probe,
    /// say) is stopped whole: nothing it started is left running.
    #[test]
    fn a_timed_out_report_leaves_no_process_behind() {
        let dir = tempfile::tempdir().unwrap();
        let pid_file = dir.path().join("grandchild.pid");
        let exe = fixture(
            &dir,
            &format!("sleep 30 &\necho $! > '{}'\nwait", pid_file.display()),
        );
        let request = request("doctor", &[], LaunchMode::Report);
        // The fixture must get as far as starting its grandchild before the
        // deadline. On a loaded machine, or the first run of a new script,
        // that can take longer than a short deadline, so each attempt allows
        // more; only a run that really started the grandchild is judged.
        let (result, pid) = [500, 2_000, 8_000]
            .into_iter()
            .find_map(|millis| {
                let _ = std::fs::remove_file(&pid_file);
                let result = run_with(&exe, &request, Duration::from_millis(millis)).unwrap();
                let pid = std::fs::read_to_string(&pid_file).unwrap_or_default();
                let pid = pid.trim().to_string();
                (!pid.is_empty()).then_some((result, pid))
            })
            .expect("the fixture never started its grandchild");
        assert!(result.timed_out);
        let alive = || {
            std::process::Command::new("kill")
                .args(["-0", &pid])
                .stderr(std::process::Stdio::null())
                .status()
                .is_ok_and(|s| s.success())
        };
        let deadline = std::time::Instant::now() + Duration::from_secs(3);
        while alive() && std::time::Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(20));
        }
        assert!(!alive(), "grandchild {pid} is still running");

        // And the next report runs at once.
        let (again, _dir) = run("echo ok", &[], LaunchMode::Report);
        assert_eq!(again.stdout, b"ok\n");
    }

    #[test]
    fn a_report_shows_both_streams_and_its_status() {
        let (result, _dir) = run("echo out; echo err >&2; exit 3", &[], LaunchMode::Report);
        let report = outcome(&request("doctor", &["doctor"], LaunchMode::Report), &result);
        assert_eq!(
            report,
            Some(Outcome::Report {
                output: "out\n".into(),
                errors: "err\n".into(),
                code: Some(3),
                truncated: false,
            })
        );
    }

    /// Home shows a known update itself; a feature it opens must not print
    /// the same notice over its own screen, or start a check of its own.
    #[test]
    fn features_opened_from_home_never_announce_updates() {
        let (result, _dir) = run(
            r#"printf '%s' "$SUVADU_NO_UPDATE_CHECK""#,
            &[],
            LaunchMode::Report,
        );
        assert_eq!(result.stdout, b"1");
    }

    #[test]
    fn features_know_home_launched_them() {
        let (result, _dir) = run(
            &format!(r#"printf '%s' "${LAUNCHED_BY_HOME}""#),
            &[],
            LaunchMode::Report,
        );
        assert_eq!(result.stdout, b"1");
    }

    #[test]
    fn a_search_with_no_history_says_so_instead_of_selecting_nothing() {
        let Some(Outcome::Failed { message, retry }) = selection("exit 11") else {
            panic!()
        };
        assert!(
            message.contains("No commands are recorded yet"),
            "{message}"
        );
        assert!(message.contains("Shell integration"), "{message}");
        assert!(!retry);
    }

    #[test]
    fn a_screen_that_closes_normally_needs_nothing_shown() {
        let (result, _dir) = run("exit 0", &[], LaunchMode::Interactive);
        let screen = request("settings", &["settings"], LaunchMode::Interactive);
        assert_eq!(outcome(&screen, &result), None);
        let (result, _dir) = run("exit 1", &[], LaunchMode::Interactive);
        assert!(matches!(
            outcome(&screen, &result),
            Some(Outcome::Failed { .. })
        ));
    }
}
