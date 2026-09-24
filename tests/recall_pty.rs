//! Recall under a real terminal, with stdout captured.
//!
//! The shell widget runs recall inside a command substitution
//! (`selected="$("$_SUVADU_BIN" "$@")"`), so stdout is a pipe while the
//! terminal is still reachable on `/dev/tty`. Inline recall has to ask the
//! terminal where the cursor is; these tests pin down both answers to that
//! question — a terminal that replies, and one that stays silent — because
//! neither can be exercised without a pty.

#![cfg(unix)]
// Driving a pty means openpty/ioctl/select; there is no safe wrapper for them
// in the standard library, and a real terminal is the only way to exercise
// the cursor-report path at all.
#![allow(unsafe_code)]

use std::io::Read;
use std::os::unix::io::{FromRawFd, RawFd};
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// The command planted in the database and selected in the UI.
const PLANTED: &str = "echo pty-recall";

/// A fragment of the planted command that styling cannot split, so it is a
/// reliable sign that a frame has been drawn.
const DRAWN_MARKER: &str = "pty-recall";

/// Give up on the child long before the harness itself is killed.
const RUN_TIMEOUT: Duration = Duration::from_secs(20);

/// How long to wait before looking at the terminal again.
const POLL_INTERVAL: Duration = Duration::from_millis(5);

/// What the terminal saw, and what the shell would have captured.
struct Recall {
    /// Everything written to the terminal, escape sequences included.
    terminal: String,
    /// Everything written to stdout — what the shell wrapper substitutes.
    stdout: String,
    success: bool,
    /// How many cursor queries the harness answered.
    answered: usize,
}

impl Recall {
    fn entered_alternate_screen(&self) -> bool {
        self.terminal.contains("\u{1b}[?1049h")
    }
}

/// Open a pty pair sized like an ordinary terminal window.
fn open_pty() -> (RawFd, RawFd) {
    let mut master: RawFd = -1;
    let mut slave: RawFd = -1;
    let mut size = libc::winsize {
        ws_row: 30,
        ws_col: 100,
        ws_xpixel: 0,
        ws_ypixel: 0,
    };
    // SAFETY: both fds are out-parameters we own, and `size` outlives the call.
    let rc = unsafe {
        libc::openpty(
            &raw mut master,
            &raw mut slave,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            &raw mut size,
        )
    };
    assert_eq!(rc, 0, "openpty failed: {}", std::io::Error::last_os_error());
    // The child must not inherit the master side, or reads never see EOF.
    // SAFETY: `master` is a valid fd we just opened.
    unsafe { libc::fcntl(master, libc::F_SETFD, libc::FD_CLOEXEC) };
    (master, slave)
}

/// Make reads on `fd` return instead of blocking.
///
/// `poll` is not an option: on macOS it answers POLLNVAL for terminal
/// devices, so readiness cannot be waited on portably here.
fn set_nonblocking(fd: RawFd) {
    // SAFETY: `fd` is open and `fcntl` touches no memory of ours.
    unsafe {
        let flags = libc::fcntl(fd, libc::F_GETFL);
        assert!(flags >= 0, "F_GETFL failed");
        assert!(
            libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) >= 0,
            "F_SETFL failed"
        );
    }
}

/// Record one command so recall has something to show.
fn plant(home: &Path) {
    let status = Command::new(env!("CARGO_BIN_EXE_suv"))
        .args([
            "add",
            "--session-id",
            "pty-session",
            "--command",
            PLANTED,
            "--cwd",
            "/tmp",
            "--exit-code",
            "0",
            "--started-at",
            "1000",
            "--ended-at",
            "1001",
        ])
        .envs(env_for(home))
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .unwrap();
    assert!(status.success(), "planting the history entry failed");
}

fn env_for(home: &Path) -> Vec<(String, String)> {
    vec![
        ("HOME".into(), home.display().to_string()),
        (
            "XDG_DATA_HOME".into(),
            home.join("data").display().to_string(),
        ),
        (
            "XDG_CONFIG_HOME".into(),
            home.join("config").display().to_string(),
        ),
        ("TERM".into(), "xterm-256color".into()),
        ("NO_COLOR".into(), "1".into()),
    ]
}

/// Run recall attached to a pty, acting as the terminal on the other side.
///
/// When `answer_cursor_query` is set we reply to `ESC[6n` the way a real
/// terminal does; otherwise we stay silent, standing in for the terminals
/// and multiplexers that ignore the request.
/// One run of recall under a pty.
struct Scenario<'a> {
    args: &'a [&'a str],
    /// Reply to `ESC[6n` the way a real terminal does.
    answer_cursor_query: bool,
    /// Bytes delivered to the terminal *before* the cursor query is answered,
    /// standing in for someone who types while recall is still measuring.
    type_before_answer: &'a str,
    /// Text whose appearance means a frame has been drawn and the UI can
    /// take a keypress.
    ready_marker: &'a str,
    /// How many cursor queries to answer before going silent. Recall asks
    /// once to open an inline viewport and again to erase it on the way out,
    /// so `1` is a terminal that stops responding during cleanup.
    max_answers: usize,
}

fn run_recall(home: &Path, scenario: &Scenario) -> Recall {
    let Scenario {
        args,
        answer_cursor_query,
        type_before_answer,
        ready_marker,
        max_answers,
    } = *scenario;
    let stdout_path = home.join("stdout.txt");
    let stdout_file = std::fs::File::create(&stdout_path).unwrap();
    let (master, slave) = open_pty();

    // SAFETY: `slave` stays open until after the child is spawned; the dups
    // are handed to the child as its stdin and stderr.
    let (child_stdin, child_stderr) = unsafe {
        (
            Stdio::from_raw_fd(libc::dup(slave)),
            Stdio::from_raw_fd(libc::dup(slave)),
        )
    };

    let mut command = Command::new(env!("CARGO_BIN_EXE_suv"));
    command
        .arg("search")
        .args(args)
        .envs(env_for(home))
        .current_dir(home)
        .stdin(child_stdin)
        .stdout(Stdio::from(stdout_file))
        .stderr(child_stderr);
    // SAFETY: only async-signal-safe calls between fork and exec. The child
    // needs its own session with the pty as controlling terminal, so that
    // opening /dev/tty reaches this pty.
    unsafe {
        command.pre_exec(|| {
            libc::setsid();
            libc::ioctl(0, libc::TIOCSCTTY.into(), 0);
            Ok(())
        });
    }
    let mut child = command.spawn().unwrap();
    // SAFETY: the child holds its own dups; the parent's copy is done.
    unsafe { libc::close(slave) };

    set_nonblocking(master);
    // SAFETY: the master fd is ours alone and closed when this file drops.
    let mut terminal_side = unsafe { std::fs::File::from_raw_fd(master) };
    let mut seen = Vec::new();
    let mut answered = 0usize;
    let mut selected = false;
    let deadline = Instant::now() + RUN_TIMEOUT;
    let status = loop {
        if Instant::now() > deadline {
            let _ = child.kill();
            break None;
        }
        let mut chunk = [0u8; 4096];
        match terminal_side.read(&mut chunk) {
            Ok(0) | Err(_) => std::thread::sleep(POLL_INTERVAL),
            Ok(read) => seen.extend_from_slice(&chunk[..read]),
        }
        // Answer every cursor query, counting against the whole stream so a
        // query split across two reads is not missed.
        let queries = seen.windows(4).filter(|w| *w == b"\x1b[6n").count();
        while answered < queries {
            // Anything typed now arrives while recall is reading the reply.
            if !type_before_answer.is_empty() {
                write_all(master, type_before_answer.as_bytes());
            }
            if answer_cursor_query && answered < max_answers {
                write_all(master, b"\x1b[12;1R");
            }
            answered += 1;
        }
        // Once the planted command is on screen the UI is drawn and ready to
        // accept the selection, whichever surface it ended up on.
        if !selected && String::from_utf8_lossy(&seen).contains(ready_marker) {
            write_all(master, b"\r");
            selected = true;
        }
        if let Some(status) = child.try_wait().unwrap() {
            break Some(status);
        }
    };

    Recall {
        terminal: String::from_utf8_lossy(&seen).into_owned(),
        stdout: std::fs::read_to_string(&stdout_path).unwrap_or_default(),
        success: status.is_some_and(|s| s.success()),
        answered,
    }
}

fn write_all(fd: RawFd, bytes: &[u8]) {
    // SAFETY: `fd` is the open master side; the slice outlives the call.
    unsafe {
        libc::write(fd, bytes.as_ptr().cast::<libc::c_void>(), bytes.len());
    }
}

#[test]
fn compact_recall_draws_inline_when_the_terminal_reports_the_cursor() {
    let home = tempfile::tempdir().unwrap();
    plant(home.path());

    let recall = run_recall(
        home.path(),
        &Scenario {
            args: &["--compact", "--query", "pty-recall"],
            answer_cursor_query: true,
            type_before_answer: "",
            ready_marker: DRAWN_MARKER,
            max_answers: usize::MAX,
        },
    );

    assert!(
        recall.success,
        "recall exited with an error; terminal saw: {}",
        recall.terminal
    );
    assert_eq!(
        recall.stdout.trim(),
        PLANTED,
        "the selected command must reach stdout for the shell wrapper"
    );
    assert!(
        !recall.entered_alternate_screen(),
        "compact recall must stay inline when the cursor position is known; answered {} queries, terminal saw: {}",
        recall.answered,
        recall.terminal.escape_debug()
    );
}

#[test]
fn compact_recall_still_returns_a_selection_when_the_terminal_stays_silent() {
    let home = tempfile::tempdir().unwrap();
    plant(home.path());

    let recall = run_recall(
        home.path(),
        &Scenario {
            args: &["--compact", "--query", "pty-recall"],
            answer_cursor_query: false,
            type_before_answer: "",
            ready_marker: DRAWN_MARKER,
            max_answers: usize::MAX,
        },
    );

    assert!(
        recall.entered_alternate_screen(),
        "recall must fall back to the full screen; terminal saw: {}",
        recall.terminal
    );
    assert!(
        recall.terminal.contains("Opening full screen instead"),
        "the fallback must say why; terminal saw: {}",
        recall.terminal
    );
    assert!(
        recall.success,
        "the fallback must not fail; terminal saw: {}",
        recall.terminal
    );
    assert_eq!(
        recall.stdout.trim(),
        PLANTED,
        "the selection must survive the fallback and reach the shell wrapper"
    );
}

#[test]
fn characters_typed_while_the_cursor_is_measured_reach_the_query() {
    let home = tempfile::tempdir().unwrap();
    plant(home.path());

    // Plain typing, the simplest case. The outcome is asserted through the
    // returned command rather than the drawn text: ratatui diffs frames and
    // writes one changed cell at a time, so typed text never appears as a
    // contiguous string in the byte stream.
    let recall = run_recall(
        home.path(),
        &Scenario {
            args: &["--compact", "--query", "pty-recall"],
            answer_cursor_query: true,
            type_before_answer: NO_MATCH,
            ready_marker: "SUVADU SEARCH",
            max_answers: usize::MAX,
        },
    );

    assert_eq!(
        recall.stdout.trim(),
        "",
        "typing during the measurement must narrow the query to nothing; \
         swallowing it returns the planted command instead. Terminal saw: {}",
        recall.terminal.escape_debug()
    );
}

#[test]
fn an_accepted_command_survives_a_terminal_that_stops_answering() {
    let home = tempfile::tempdir().unwrap();
    plant(home.path());

    // Answer the measurement that opens the inline viewport, then go silent.
    // Erasing that viewport on the way out asks a second time. By then the
    // command has already been accepted, so a cleanup failure must not turn
    // it into an error with nothing on the prompt.
    let recall = run_recall(
        home.path(),
        &Scenario {
            args: &["--compact", "--query", "pty-recall"],
            answer_cursor_query: true,
            type_before_answer: "",
            ready_marker: DRAWN_MARKER,
            max_answers: 1,
        },
    );

    assert_eq!(
        recall.stdout.trim(),
        PLANTED,
        "the accepted command must still reach the shell; terminal saw: {}",
        recall.terminal.escape_debug()
    );
    assert!(
        recall.success,
        "recall must not exit with an error after accepting a command"
    );
}

/// Text that matches nothing planted, so a query that really arrived is
/// visible in the outcome: no selection instead of the planted command.
const NO_MATCH: &str = "zzzznomatch";

/// Bracketed paste, as a terminal sends it.
fn pasted(text: &str) -> String {
    format!("\x1b[200~{text}\x1b[201~")
}

#[test]
fn a_paste_during_opening_reaches_the_query() {
    let home = tempfile::tempdir().unwrap();
    plant(home.path());

    let recall = run_recall(
        home.path(),
        &Scenario {
            args: &["--compact", "--query", "pty-recall"],
            answer_cursor_query: true,
            type_before_answer: &pasted(NO_MATCH),
            ready_marker: "SUVADU SEARCH",
            max_answers: usize::MAX,
        },
    );

    assert_eq!(
        recall.stdout.trim(),
        "",
        "the pasted text matches nothing, so Enter must return no command; terminal saw: {}",
        recall.terminal.escape_debug()
    );
}

#[test]
fn editing_keys_during_opening_are_not_swallowed() {
    let home = tempfile::tempdir().unwrap();
    plant(home.path());

    // Backspace first: it applies to an empty query, so everything after it
    // is still ordinary typing. Dropping the rest of the buffer at the first
    // control byte would lose all of it.
    let typed = format!("\x7f{NO_MATCH}");
    let recall = run_recall(
        home.path(),
        &Scenario {
            args: &["--compact", "--query", "pty-recall"],
            answer_cursor_query: true,
            type_before_answer: &typed,
            ready_marker: "SUVADU SEARCH",
            max_answers: usize::MAX,
        },
    );

    assert_eq!(
        recall.stdout.trim(),
        "",
        "the typing after Backspace must reach the query; terminal saw: {}",
        recall.terminal.escape_debug()
    );
}

#[test]
fn typing_during_the_fallback_wait_reaches_the_query() {
    let home = tempfile::tempdir().unwrap();
    plant(home.path());

    // The terminal never answers, so recall waits, gives up and opens full
    // screen. What was typed during that wait belongs in the query.
    let recall = run_recall(
        home.path(),
        &Scenario {
            args: &["--compact", "--query", "pty-recall"],
            answer_cursor_query: false,
            type_before_answer: NO_MATCH,
            ready_marker: "SUVADU SEARCH",
            max_answers: usize::MAX,
        },
    );

    assert!(
        recall.entered_alternate_screen(),
        "this case must fall back to full screen"
    );
    assert_eq!(
        recall.stdout.trim(),
        "",
        "typing during the wait must reach the query; terminal saw: {}",
        recall.terminal.escape_debug()
    );
}
