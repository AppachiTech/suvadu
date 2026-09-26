//! Home under a real terminal: opening features, coming back, and leaving
//! the terminal as it was found.
//!
//! Each test runs `suv home` on a pty with a private HOME, reads what it
//! draws through a small screen emulator (so assertions see the screen, not
//! a stream of cursor moves), and presses keys the way a person would.

#![cfg(unix)]
// Driving a pty means openpty/ioctl/tcgetattr; there is no safe wrapper for
// them in the standard library.
#![allow(unsafe_code)]

use std::io::Read;
use std::os::unix::io::{FromRawFd, RawFd};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::time::{Duration, Instant};

use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

const WAIT: Duration = Duration::from_secs(20);
const ESC: &[u8] = b"\x1b";
const ENTER: &[u8] = b"\r";

// ── A small screen emulator ─────────────────────────────────────

/// Enough of a terminal to read ratatui's and crossterm's output back as
/// rows of text: cursor moves, erases, the alternate screen, and wide
/// characters. Colours and modes are parsed and ignored.
struct Screen {
    cols: usize,
    rows: usize,
    main: Vec<Vec<String>>,
    alt: Vec<Vec<String>>,
    in_alt: bool,
    row: usize,
    col: usize,
    pending: Vec<u8>,
}

impl Screen {
    fn new(cols: u16, rows: u16) -> Self {
        let (cols, rows) = (usize::from(cols), usize::from(rows));
        Self {
            cols,
            rows,
            main: vec![vec![" ".to_string(); cols]; rows],
            alt: vec![vec![" ".to_string(); cols]; rows],
            in_alt: false,
            row: 0,
            col: 0,
            pending: Vec::new(),
        }
    }

    fn resize(&mut self, cols: u16, rows: u16) {
        let (cols, rows) = (usize::from(cols), usize::from(rows));
        for grid in [&mut self.main, &mut self.alt] {
            grid.resize(rows, vec![" ".to_string(); cols]);
            for line in grid.iter_mut() {
                line.resize(cols, " ".to_string());
            }
        }
        self.cols = cols;
        self.rows = rows;
        self.row = self.row.min(rows.saturating_sub(1));
        self.col = self.col.min(cols.saturating_sub(1));
    }

    const fn grid(&mut self) -> &mut Vec<Vec<String>> {
        if self.in_alt {
            &mut self.alt
        } else {
            &mut self.main
        }
    }

    fn text(&self) -> String {
        let grid = if self.in_alt { &self.alt } else { &self.main };
        grid.iter()
            .map(|line| line.concat().trim_end().to_string())
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn feed(&mut self, bytes: &[u8]) {
        self.pending.extend_from_slice(bytes);
        let data = std::mem::take(&mut self.pending);
        let mut i = 0;
        while i < data.len() {
            if data[i] == 0x1b {
                let Some(used) = self.escape(&data[i..]) else {
                    self.pending = data[i..].to_vec();
                    return;
                };
                i += used;
                continue;
            }
            // Printable text up to the next escape, as UTF-8.
            let end = data[i..]
                .iter()
                .position(|b| *b == 0x1b)
                .map_or(data.len(), |p| i + p);
            let chunk = &data[i..end];
            let valid = match std::str::from_utf8(chunk) {
                Ok(s) => s.len(),
                Err(e) if e.error_len().is_none() && end == data.len() => e.valid_up_to(),
                Err(e) => e.valid_up_to() + e.error_len().unwrap_or(1),
            };
            let text = String::from_utf8_lossy(&chunk[..valid]).into_owned();
            self.print(&text);
            i += valid;
            if valid < chunk.len() && end == data.len() {
                self.pending = data[i..].to_vec();
                return;
            }
        }
    }

    fn print(&mut self, text: &str) {
        for g in text.graphemes(true) {
            match g {
                "\r" => self.col = 0,
                "\n" | "\r\n" => {
                    if g == "\r\n" {
                        self.col = 0;
                    }
                    if self.row + 1 < self.rows {
                        self.row += 1;
                    } else {
                        let cols = self.cols;
                        let grid = self.grid();
                        grid.remove(0);
                        grid.push(vec![" ".to_string(); cols]);
                    }
                }
                "\u{8}" => self.col = self.col.saturating_sub(1),
                "\u{7}" => {}
                g => {
                    let width = g.width();
                    if width == 0 || self.row >= self.rows {
                        continue;
                    }
                    if self.col + width > self.cols {
                        continue;
                    }
                    let (row, col) = (self.row, self.col);
                    let grid = self.grid();
                    grid[row][col] = g.to_string();
                    for extra in 1..width {
                        grid[row][col + extra] = String::new();
                    }
                    self.col += width;
                }
            }
        }
    }

    /// Handle one escape sequence at the start of `data`; the bytes used,
    /// or `None` when it is incomplete.
    fn escape(&mut self, data: &[u8]) -> Option<usize> {
        let next = *data.get(1)?;
        match next {
            b'[' => {
                let end = data[2..].iter().position(|b| (0x40..=0x7e).contains(b))? + 2;
                let params = std::str::from_utf8(&data[2..end]).unwrap_or("");
                self.csi(params, data[end]);
                Some(end + 1)
            }
            b']' => {
                let body = &data[2..];
                let bell = body.iter().position(|b| *b == 0x07);
                let st = body.windows(2).position(|w| w == b"\x1b\\");
                match (bell, st) {
                    (Some(b), Some(s)) if s < b => Some(2 + s + 2),
                    (Some(b), _) => Some(2 + b + 1),
                    (None, Some(s)) => Some(2 + s + 2),
                    (None, None) => None,
                }
            }
            _ => Some(2),
        }
    }

    fn csi(&mut self, params: &str, command: u8) {
        let private = params.starts_with('?');
        let numbers: Vec<usize> = params
            .trim_start_matches('?')
            .split(';')
            .map(|n| n.parse().unwrap_or(0))
            .collect();
        let first = numbers.first().copied().unwrap_or(0);
        let at_least_one = first.max(1);
        let blank = || " ".to_string();
        match command {
            b'H' | b'f' => {
                self.row = at_least_one
                    .saturating_sub(1)
                    .min(self.rows.saturating_sub(1));
                let col = numbers.get(1).copied().unwrap_or(1).max(1);
                self.col = (col - 1).min(self.cols.saturating_sub(1));
            }
            b'A' => self.row = self.row.saturating_sub(at_least_one),
            b'B' => self.row = (self.row + at_least_one).min(self.rows.saturating_sub(1)),
            b'C' => self.col = (self.col + at_least_one).min(self.cols.saturating_sub(1)),
            b'D' => self.col = self.col.saturating_sub(at_least_one),
            b'G' => self.col = (at_least_one - 1).min(self.cols.saturating_sub(1)),
            b'd' => self.row = (at_least_one - 1).min(self.rows.saturating_sub(1)),
            b'J' => {
                let (row, col, cols) = (self.row, self.col, self.cols);
                let grid = self.grid();
                match first {
                    2 | 3 => grid
                        .iter_mut()
                        .for_each(|l| l.iter_mut().for_each(|c| *c = blank())),
                    0 => {
                        for c in &mut grid[row][col..] {
                            *c = blank();
                        }
                        for line in grid.iter_mut().skip(row + 1) {
                            *line = vec![blank(); cols];
                        }
                    }
                    _ => {}
                }
            }
            b'K' => {
                let (row, col) = (self.row, self.col);
                if let Some(line) = self.grid().get_mut(row) {
                    let range = match first {
                        1 => 0..=col.min(line.len().saturating_sub(1)),
                        2 => 0..=line.len().saturating_sub(1),
                        _ => col.min(line.len())..=line.len().saturating_sub(1),
                    };
                    for c in &mut line[range] {
                        *c = blank();
                    }
                }
            }
            b'h' | b'l' if private && numbers.contains(&1049) => {
                let entering = command == b'h';
                if entering && !self.in_alt {
                    self.alt = vec![vec![blank(); self.cols]; self.rows];
                }
                self.in_alt = entering;
            }
            _ => {}
        }
    }
}

// ── A session on a pty ──────────────────────────────────────────

struct Session {
    master: std::fs::File,
    master_fd: RawFd,
    child: Child,
    screen: Screen,
    output: Vec<u8>,
    status: Option<ExitStatus>,
}

fn private_env(home: &Path) -> Vec<(String, String)> {
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
        ("PATH".into(), "/usr/bin:/bin".into()),
    ]
}

/// Record one command in the private history.
fn plant(home: &Path, command: &str) {
    let status = Command::new(env!("CARGO_BIN_EXE_suv"))
        .args([
            "add",
            "--session-id",
            "pty-home",
            "--command",
            command,
            "--cwd",
            "/tmp",
            "--exit-code",
            "0",
            "--started-at",
            "1000",
            "--ended-at",
            "1001",
        ])
        .env_clear()
        .envs(private_env(home))
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .unwrap();
    assert!(status.success(), "planting the history entry failed");
}

fn set_size(fd: RawFd, cols: u16, rows: u16) {
    let size = libc::winsize {
        ws_row: rows,
        ws_col: cols,
        ws_xpixel: 0,
        ws_ypixel: 0,
    };
    // SAFETY: `fd` is an open pty descriptor and `size` outlives the call.
    unsafe { libc::ioctl(fd, libc::TIOCSWINSZ, &raw const size) };
}

impl Session {
    fn start(home: &Path, args: &[&str]) -> Self {
        let (cols, rows) = (100, 30);
        let mut master: RawFd = -1;
        let mut slave: RawFd = -1;
        // SAFETY: both fds are out-parameters we own.
        let rc = unsafe {
            libc::openpty(
                &raw mut master,
                &raw mut slave,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
            )
        };
        assert_eq!(rc, 0, "openpty failed");
        set_size(master, cols, rows);
        // SAFETY: `master` is a valid fd we just opened.
        unsafe {
            libc::fcntl(master, libc::F_SETFD, libc::FD_CLOEXEC);
            libc::fcntl(master, libc::F_SETFL, libc::O_NONBLOCK);
        }
        // A shell leads the session and runs suv, so that once suv exits
        // the same terminal can report its modes (a session leader's exit
        // revokes the terminal on some systems). The shell exits with suv's
        // status.
        let mut command = Command::new("/bin/sh");
        command.args([
            "-c",
            "\"$0\" \"$@\"; status=$?; printf '\\n@@MODES@@ '; stty -a | tr '\\n' ' '; printf '@@END@@'; exit $status",
            env!("CARGO_BIN_EXE_suv"),
        ]);
        // SAFETY: the dups are handed to the child as its standard streams.
        let (stdin, stdout, stderr) = unsafe {
            (
                Stdio::from_raw_fd(libc::dup(slave)),
                Stdio::from_raw_fd(libc::dup(slave)),
                Stdio::from_raw_fd(libc::dup(slave)),
            )
        };
        command
            .args(args)
            .env_clear()
            .envs(private_env(home))
            .current_dir(home)
            .stdin(stdin)
            .stdout(stdout)
            .stderr(stderr);
        // SAFETY: only async-signal-safe calls between fork and exec; the
        // child needs the pty as its controlling terminal.
        unsafe {
            command.pre_exec(|| {
                libc::setsid();
                libc::ioctl(0, libc::TIOCSCTTY.into(), 0);
                Ok(())
            });
        }
        let child = command.spawn().unwrap();
        // SAFETY: the child holds its own dups; the parent's copy is done.
        unsafe { libc::close(slave) };
        Self {
            // SAFETY: the master fd is ours alone and closed when this drops.
            master: unsafe { std::fs::File::from_raw_fd(master) },
            master_fd: master,
            child,
            screen: Screen::new(cols, rows),
            output: Vec::new(),
            status: None,
        }
    }

    fn pump(&mut self) {
        let mut chunk = vec![0u8; 16 * 1024];
        loop {
            match self.master.read(&mut chunk) {
                Ok(0) | Err(_) => break,
                Ok(read) => {
                    self.output.extend_from_slice(&chunk[..read]);
                    self.screen.feed(&chunk[..read]);
                }
            }
        }
        if self.status.is_none() {
            self.status = self.child.try_wait().unwrap();
        }
    }

    fn wait_for(&mut self, what: &str, done: impl Fn(&str) -> bool) {
        let deadline = Instant::now() + WAIT;
        loop {
            self.pump();
            if done(&self.screen.text()) {
                return;
            }
            if Instant::now() > deadline || self.status.is_some() {
                let _ = self.child.kill();
                panic!(
                    "waiting for {what}; exited: {:?}\n--- screen ---\n{}",
                    self.status,
                    self.screen.text()
                );
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    fn wait_text(&mut self, text: &str) {
        self.wait_for(text, |screen| screen.contains(text));
    }

    fn send(&mut self, bytes: &[u8]) {
        // SAFETY: `master_fd` is open for the life of the session.
        let written = unsafe { libc::write(self.master_fd, bytes.as_ptr().cast(), bytes.len()) };
        if usize::try_from(written).ok() != Some(bytes.len()) {
            self.pump();
            assert!(
                self.status.is_some(),
                "could not type into a running program"
            );
        }
        // Let a lone Esc arrive on its own, as a keypress would — reading
        // the screen meanwhile, as a terminal does, so drawing never stalls.
        let until = Instant::now() + Duration::from_millis(120);
        while Instant::now() < until {
            self.pump();
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    fn resize(&mut self, cols: u16, rows: u16) {
        set_size(self.master_fd, cols, rows);
        self.screen.resize(cols, rows);
    }

    fn wait_exit(&mut self) -> ExitStatus {
        let deadline = Instant::now() + WAIT;
        loop {
            self.pump();
            if let Some(status) = self.status {
                return status;
            }
            if Instant::now() > deadline {
                let _ = self.child.kill();
                panic!("did not exit\n--- screen ---\n{}", self.screen.text());
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    /// Canonical input and echo are on again after suv exits: the shell
    /// can read a line. Read from `stty -a`, run on the same terminal.
    fn terminal_is_cooked(&self) -> bool {
        let output = String::from_utf8_lossy(&self.output);
        let modes = output
            .split("@@MODES@@")
            .nth(1)
            .and_then(|rest| rest.split("@@END@@").next())
            .unwrap_or_else(|| panic!("no terminal modes reported:\n{output}"));
        let words: Vec<&str> = modes.split_whitespace().collect();
        words.contains(&"icanon") && words.contains(&"echo")
    }

    fn written(&self, sequence: &str) -> usize {
        String::from_utf8_lossy(&self.output)
            .matches(sequence)
            .count()
    }

    /// Everything the program drew was taken down again.
    fn left_the_terminal_as_found(&self) {
        assert!(self.terminal_is_cooked(), "raw mode left on");
        assert_eq!(
            self.written("\u{1b}[?1049h"),
            self.written("\u{1b}[?1049l"),
            "alternate screen entered and left unevenly"
        );
        assert_eq!(
            self.written("\u{1b}[?2004h"),
            self.written("\u{1b}[?2004l"),
            "bracketed paste enabled and disabled unevenly"
        );
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Open Home, ready for keys.
fn home() -> (tempfile::TempDir, Session) {
    let dir = tempfile::tempdir().unwrap();
    let mut session = Session::start(dir.path(), &["home"]);
    session.wait_text("SUVADU HOME");
    session.wait_text("Find a command");
    (dir, session)
}

fn type_text(session: &mut Session, text: &str) {
    session.send(text.as_bytes());
}

/// Press Esc until Home exits, giving each press time to take effect.
fn quit(session: &mut Session) -> ExitStatus {
    for _ in 0..5 {
        session.send(ESC);
        let until = Instant::now() + Duration::from_millis(600);
        while Instant::now() < until {
            session.pump();
            if session.status.is_some() {
                return session.wait_exit();
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }
    session.wait_exit()
}

// ── Tests ───────────────────────────────────────────────────────

#[test]
fn home_opens_and_esc_leaves_the_terminal_as_found() {
    let (_dir, mut session) = home();
    let screen = session.screen.text();
    assert!(screen.contains("Review AI activity"), "{screen}");
    assert!(screen.contains("Command reference"), "{screen}");
    let status = quit(&mut session);
    assert!(status.success(), "{status:?}");
    session.left_the_terminal_as_found();
}

#[test]
fn ctrl_c_quits_with_the_interrupted_status() {
    let (_dir, mut session) = home();
    type_text(&mut session, "backup");
    session.send(b"\x03");
    let status = session.wait_exit();
    assert_eq!(status.code(), Some(130));
    session.left_the_terminal_as_found();
}

#[test]
fn a_guide_opens_and_back_returns_to_the_same_search() {
    let (_dir, mut session) = home();
    type_text(&mut session, "backup");
    session.wait_text("Features matching “backup”");
    session.send(ENTER);
    session.wait_text("Copy example");
    assert!(session
        .screen
        .text()
        .contains("There is no restore command"));
    session.send(ESC);
    session.wait_for("the search again", |s| {
        s.contains("Features matching “backup”") && !s.contains("Copy example")
    });
    assert!(quit(&mut session).success());
    session.left_the_terminal_as_found();
}

#[test]
fn the_reference_opens_scrolls_and_closes() {
    let (_dir, mut session) = home();
    session.send(b"\x1bOQ"); // F2
    session.wait_text("Command reference");
    session.wait_text("Usage: suv <COMMAND>");
    session.send(b"\t");
    session.send(b"\x1b[6~"); // PageDown
    session.send(ESC);
    session.send(ESC);
    session.wait_for("Home again", |s| {
        s.contains("Find a command") && s.contains("Explore")
    });
    assert!(quit(&mut session).success());
    session.left_the_terminal_as_found();
}

#[test]
fn settings_opens_and_esc_returns_to_the_same_selection() {
    let (dir, mut session) = home();
    type_text(&mut session, "settings");
    session.wait_text("Features matching “settings”");
    session.send(ENTER);
    session.wait_text("SUVADU SETTINGS");
    // Stay long enough that Home does not ask for a key on the way back.
    std::thread::sleep(Duration::from_millis(1200));
    session.send(ESC);
    session.wait_for("Home with Settings still selected", |s| {
        s.contains("SUVADU HOME")
            && s.contains("Features matching “settings”")
            && s.contains("Open settings")
    });
    assert!(quit(&mut session).success());
    session.left_the_terminal_as_found();
    assert!(
        !dir.path()
            .join("Library/Application Support/tech.appachi.suvadu/history.db")
            .exists()
            && !dir.path().join("data/suvadu/history.db").exists(),
        "opening settings from Home created a database"
    );
}

#[test]
fn a_cancelled_search_selects_nothing_and_returns() {
    let dir = tempfile::tempdir().unwrap();
    plant(dir.path(), "echo planted-for-home");
    let mut session = Session::start(dir.path(), &["home"]);
    session.wait_text("SUVADU HOME");
    for round in 0..2 {
        session.send(b"\x12"); // Ctrl+R
        session.wait_text("planted-for-home");
        std::thread::sleep(Duration::from_millis(1200));
        session.send(ESC);
        session.wait_text("Nothing was selected.");
        session.send(ENTER); // Back to Home
        session.wait_for(&format!("Home after round {round}"), |s| {
            s.contains("Explore") && !s.contains("Nothing was selected.")
        });
    }
    assert!(quit(&mut session).success());
    session.left_the_terminal_as_found();
}

/// A picked command is shown to copy — and never run, even when it would
/// leave evidence if it were.
#[test]
fn a_picked_command_is_shown_exactly_and_never_run() {
    let dir = tempfile::tempdir().unwrap();
    let sentinel: PathBuf = dir.path().join("sentinel");
    let planted = format!("touch {}", sentinel.display());
    plant(dir.path(), &planted);
    let mut session = Session::start(dir.path(), &["home"]);
    session.wait_text("SUVADU HOME");
    session.send(b"\x12"); // Ctrl+R
    session.wait_text("sentinel");
    session.send(ENTER);
    session.wait_text("Selected command");
    let screen = session.screen.text();
    assert!(screen.contains("This has not been run."), "{screen}");
    assert!(screen.contains("Copy command"), "{screen}");
    assert!(screen.contains(&planted), "{screen}");
    session.send(b"\x1b[C"); // Right: Back to Home
    session.send(ENTER);
    session.wait_for("Home", |s| {
        s.contains("Explore") && !s.contains("Selected command")
    });
    assert!(quit(&mut session).success());
    session.left_the_terminal_as_found();
    assert!(!sentinel.exists(), "the picked command was run");
}

#[test]
fn a_failing_feature_is_explained_and_home_comes_back() {
    let dir = tempfile::tempdir().unwrap();
    // A file where the data directory should be: anything that opens the
    // database fails, while Home itself needs nothing from it.
    for blocked in [
        dir.path()
            .join("Library/Application Support/tech.appachi.suvadu"),
        dir.path().join("data/suvadu"),
    ] {
        std::fs::create_dir_all(blocked.parent().unwrap()).unwrap();
        std::fs::write(&blocked, "not a directory").unwrap();
    }
    let mut session = Session::start(dir.path(), &["home"]);
    session.wait_text("SUVADU HOME");
    type_text(&mut session, "usage statistics");
    session.wait_text("Features matching");
    session.send(ENTER);
    session.wait_text("return to Suvadu Home");
    session.send(ENTER);
    session.wait_text("exited with status");
    session.send(ESC);
    session.wait_for("the search results again", |s| {
        s.contains("Features matching “usage statistics”") && !s.contains("exited with status")
    });
    assert!(quit(&mut session).success());
    session.left_the_terminal_as_found();
}

#[test]
fn a_report_is_shown_inside_home() {
    let (_dir, mut session) = home();
    type_text(&mut session, "version");
    session.wait_text("Features matching “version”");
    session.send(ENTER);
    session.wait_text(&format!("suvadu v{}", env!("CARGO_PKG_VERSION")));
    session.send(ENTER); // Back to Home
    session.wait_for("Home", |s| s.contains("Features matching “version”"));
    assert!(quit(&mut session).success());
    session.left_the_terminal_as_found();
}

#[test]
fn resizing_small_and_back_keeps_the_place() {
    let (_dir, mut session) = home();
    session.send(b"\x1b[B"); // Down: Review a session
    session.resize(30, 8);
    session.wait_text("too small");
    session.resize(100, 30);
    session.wait_for("Home at full size", |s| {
        s.contains("Browse sessions") && s.contains("SUVADU HOME")
    });
    assert!(quit(&mut session).success());
    session.left_the_terminal_as_found();
}

#[test]
fn a_bare_suv_with_home_chosen_opens_home() {
    let dir = tempfile::tempdir().unwrap();
    for path in [
        dir.path()
            .join("Library/Application Support/tech.appachi.suvadu/config.toml"),
        dir.path().join("config/suvadu/config.toml"),
    ] {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, "[home]\nstartup = \"home\"\n").unwrap();
    }
    let mut session = Session::start(dir.path(), &[]);
    session.wait_text("SUVADU HOME");
    assert!(quit(&mut session).success());
    session.left_the_terminal_as_found();
}

#[test]
fn a_bare_suv_without_a_choice_prints_the_overview() {
    let dir = tempfile::tempdir().unwrap();
    let mut session = Session::start(dir.path(), &[]);
    let status = session.wait_exit();
    assert!(status.success(), "{status:?}");
    let shown = String::from_utf8_lossy(&session.output).replace("\r\n", "\n");
    assert!(shown.contains("Start here:") && shown.contains("Usage: suv <COMMAND>"));
    assert_eq!(
        session.written("\u{1b}[?1049h"),
        0,
        "no screen was taken over"
    );
}
