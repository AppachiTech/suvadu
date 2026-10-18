//! The agent screens' exit status: a screen that fails after it started
//! exits non-zero, so Home's pause-on-failure and scripts can tell, while
//! leaving one normally exits 0.
//!
//! Each test runs `suv agent <screen>` on a pty with a private HOME.

#![cfg(unix)]
// Driving a pty means openpty/ioctl; there is no safe wrapper for them in
// the standard library.
#![allow(unsafe_code)]

use std::io::Read;
use std::os::unix::io::{FromRawFd, RawFd};
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::{Child, Command, ExitStatus, Stdio};
use std::time::{Duration, Instant};

const WAIT: Duration = Duration::from_secs(20);

/// `suv` reading keys from a pty and drawing into a pipe, both held by
/// this test, so the output can go away while keys still arrive.
struct Screen {
    master: std::fs::File,
    output_pipe: Option<std::fs::File>,
    child: Child,
    output: Vec<u8>,
    status: Option<ExitStatus>,
}

impl Screen {
    fn start(home: &Path, args: &[&str]) -> Self {
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
        let size = libc::winsize {
            ws_row: 30,
            ws_col: 120,
            ws_xpixel: 0,
            ws_ypixel: 0,
        };
        // SAFETY: `master` is a valid fd we just opened; `size` outlives
        // the call.
        unsafe {
            libc::ioctl(master, libc::TIOCSWINSZ, &raw const size);
            libc::fcntl(master, libc::F_SETFD, libc::FD_CLOEXEC);
            libc::fcntl(master, libc::F_SETFL, libc::O_NONBLOCK);
        }
        let mut pipe: [RawFd; 2] = [-1; 2];
        // SAFETY: `pipe` is an out-parameter we own.
        assert_eq!(unsafe { libc::pipe(pipe.as_mut_ptr()) }, 0, "pipe failed");
        // SAFETY: the read end is ours; the write end goes to the child.
        unsafe {
            libc::fcntl(pipe[0], libc::F_SETFD, libc::FD_CLOEXEC);
            libc::fcntl(pipe[0], libc::F_SETFL, libc::O_NONBLOCK);
        }
        // SAFETY: each fd is handed to the child as one standard stream.
        let (stdin, stdout, stderr) = unsafe {
            (
                Stdio::from_raw_fd(libc::dup(slave)),
                Stdio::from_raw_fd(pipe[1]),
                Stdio::from_raw_fd(libc::dup(slave)),
            )
        };
        let mut command = Command::new(env!("CARGO_BIN_EXE_suv"));
        command
            .args(args)
            .env_clear()
            .env("HOME", home)
            .env("XDG_DATA_HOME", home.join("data"))
            .env("XDG_CONFIG_HOME", home.join("config"))
            .env("TERM", "xterm-256color")
            .env("NO_COLOR", "1")
            .env("PATH", "/usr/bin:/bin")
            // No test may start a real update check over the network.
            .env("SUVADU_NO_UPDATE_CHECK", "1")
            .current_dir(home)
            .stdin(stdin)
            .stdout(stdout)
            .stderr(stderr);
        // SAFETY: only async-signal-safe calls between fork and exec. The
        // child gets the pty as its controlling terminal, as at a shell.
        unsafe {
            command.pre_exec(|| {
                libc::setsid();
                libc::ioctl(0, libc::TIOCSCTTY.into(), 0);
                Ok(())
            });
        }
        let child = command.spawn().unwrap();
        // The command owns the child's copies; drop it so ours are the
        // only ends left open here.
        drop(command);
        // SAFETY: the child holds its own dups; the parent's copy is done.
        unsafe { libc::close(slave) };
        Self {
            // SAFETY: these fds are ours alone and closed when dropped.
            master: unsafe { std::fs::File::from_raw_fd(master) },
            output_pipe: Some(unsafe { std::fs::File::from_raw_fd(pipe[0]) }),
            child,
            output: Vec::new(),
            status: None,
        }
    }

    fn pump(&mut self) {
        let mut chunk = vec![0u8; 16 * 1024];
        for stream in [Some(&mut self.master), self.output_pipe.as_mut()]
            .into_iter()
            .flatten()
        {
            loop {
                match stream.read(&mut chunk) {
                    Ok(0) | Err(_) => break,
                    Ok(read) => self.output.extend_from_slice(&chunk[..read]),
                }
            }
        }
        if self.status.is_none() {
            self.status = self.child.try_wait().unwrap();
        }
    }

    fn text(&self) -> String {
        String::from_utf8_lossy(&self.output).into_owned()
    }

    fn wait_text(&mut self, text: &str) {
        let deadline = Instant::now() + WAIT;
        loop {
            self.pump();
            if self.text().contains(text) {
                return;
            }
            assert!(
                Instant::now() < deadline && self.status.is_none(),
                "waiting for {text:?}; exited: {:?}\n{}",
                self.status,
                self.text()
            );
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    fn send(&mut self, bytes: &[u8]) {
        use std::io::Write;
        self.master.write_all(bytes).unwrap();
    }

    /// Close what the screen draws into, so its next frame fails.
    fn close_output(&mut self) {
        self.pump();
        self.output_pipe = None;
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
                panic!("did not exit\n{}", self.text());
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }
}

impl Drop for Screen {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Each agent screen with the title it draws.
const SCREENS: [(&str, &str); 3] = [
    ("dashboard", "SUVADU AGENT DASHBOARD"),
    ("prompts", "SUVADU PROMPT EXPLORER"),
    ("stats", "SUVADU AGENT STATS"),
];

#[test]
fn an_agent_screen_that_cannot_draw_exits_non_zero() {
    for (screen, title) in SCREENS {
        let home = tempfile::tempdir().unwrap();
        let mut session = Screen::start(home.path(), &["agent", screen]);
        session.wait_text(title);
        session.close_output();
        // A key makes the screen draw again, into nothing.
        session.send(b"\x1b[B");
        let status = session.wait_exit();
        assert!(
            !status.success(),
            "suv agent {screen} failed but exited {status:?}"
        );
        assert!(
            session.text().contains("Error: "),
            "suv agent {screen} gave no reason:\n{}",
            session.text()
        );
    }
}

#[test]
fn leaving_an_agent_screen_normally_exits_zero() {
    for (screen, title) in SCREENS {
        let home = tempfile::tempdir().unwrap();
        let mut session = Screen::start(home.path(), &["agent", screen]);
        session.wait_text(title);
        session.send(b"\x1b");
        let status = session.wait_exit();
        assert!(status.success(), "suv agent {screen} exited {status:?}");
    }
}
