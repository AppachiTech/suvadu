use std::process;

pub fn handle_wrap(
    command: &[String],
    executor_type: &str,
    executor: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    if command.is_empty() {
        return Err("No command provided to wrap.".into());
    }

    let cmd_str = command.join(" ");
    let cwd = std::env::current_dir()
        .map_or_else(|_| ".".to_string(), |p| p.to_string_lossy().to_string());

    let session_id =
        std::env::var("SUVADU_SESSION_ID").unwrap_or_else(|_| uuid::Uuid::new_v4().to_string());

    let started_at = chrono::Utc::now().timestamp_millis();

    // Execute the command. Its stderr goes through Suvadu only when its
    // tail could be kept; otherwise the command has the terminal itself.
    let (status, failure_output) = if keeps_failure_output(&cwd) {
        run_teeing_stderr(&cmd_str)
    } else {
        let status = std::process::Command::new("sh")
            .arg("-c")
            .arg(&cmd_str)
            .status();
        (status, None)
    };

    let ended_at = chrono::Utc::now().timestamp_millis();

    let exit_code = status.map_or(127, exit_code_from_status);

    // Record in history (log failure to stderr; don't block the wrapped command's exit)
    if let Err(e) = super::entry::handle_add_with_context(super::entry::AddParams {
        session_id,
        command: cmd_str,
        cwd,
        exit_code: Some(exit_code),
        started_at,
        ended_at,
        executor_type: Some(executor_type.to_string()),
        executor: Some(executor.to_string()),
        context: None,
        start_cwd: None,
        failure_output,
    }) {
        eprintln!("suvadu: failed to record command: {e}");
    }

    // Flush stderr before exiting — process::exit skips destructors and buffer flushes
    let _ = std::io::Write::flush(&mut std::io::stderr());
    process::exit(exit_code);
}

/// Whether a command run in `cwd` would have its failure output kept: the
/// same policy recording applies, read once more here because it decides
/// how the command is started.
fn keeps_failure_output(cwd: &str) -> bool {
    if crate::config::is_paused() {
        return false;
    }
    let config = crate::config::capture_config_for_dir(std::path::Path::new(cwd));
    config.enabled && config.capture.failure_output
}

/// How long the command's stderr is still read after it exits, for what
/// it wrote last. A background process it started may hold the pipe open
/// for good, and `suv wrap` must not wait on it.
const STDERR_DRAIN: std::time::Duration = std::time::Duration::from_millis(250);

/// Run `cmd` under `sh -c` with its stderr passed through to ours byte
/// for byte as it arrives, keeping the last window of it. The command
/// sees a pipe rather than a terminal on stderr, so a program that colours
/// or animates only on a terminal prints plainly.
fn run_teeing_stderr(
    cmd: &str,
) -> (
    std::io::Result<std::process::ExitStatus>,
    Option<crate::failure_output::Captured>,
) {
    use std::io::{Read, Write};
    use std::sync::{Arc, Mutex};

    let mut child = match std::process::Command::new("sh")
        .arg("-c")
        .arg(cmd)
        .stderr(std::process::Stdio::piped())
        .spawn()
    {
        Ok(child) => child,
        Err(e) => return (Err(e), None),
    };
    let Some(mut pipe) = child.stderr.take() else {
        return (child.wait(), None);
    };
    let tail = Arc::new(Mutex::new(crate::failure_output::TailBuffer::new()));
    let kept = Arc::clone(&tail);
    let tee = std::thread::spawn(move || {
        let mut stderr = std::io::stderr();
        let mut chunk = [0u8; 8192];
        loop {
            match pipe.read(&mut chunk) {
                Ok(0) => break,
                Ok(n) => {
                    let _ = stderr.write_all(&chunk[..n]);
                    if let Ok(mut tail) = kept.lock() {
                        tail.push(&chunk[..n]);
                    }
                }
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
                Err(_) => break,
            }
        }
    });
    let status = child.wait();
    let deadline = std::time::Instant::now() + STDERR_DRAIN;
    while !tee.is_finished() && std::time::Instant::now() < deadline {
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    let captured = tail
        .lock()
        .ok()
        .map(|mut tail| std::mem::take(&mut *tail).into_captured("stderr"));
    (status, captured)
}

/// Extract the exit code from an `ExitStatus`.
///
/// - Normal exit → the actual code (0–255).
/// - Signal-killed on Unix → 128 + signal number (standard shell convention).
/// - Fallback (no code, non-Unix) → 1.
fn exit_code_from_status(status: std::process::ExitStatus) -> i32 {
    if let Some(code) = status.code() {
        return code;
    }
    // On Unix, a None code means the process was killed by a signal.
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        if let Some(sig) = status.signal() {
            return 128 + sig;
        }
    }
    1
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_exit_code_normal_exit() {
        // A process that exits normally with code 0
        let status = std::process::Command::new("sh")
            .arg("-c")
            .arg("exit 0")
            .status()
            .unwrap();
        assert_eq!(exit_code_from_status(status), 0);
    }

    #[test]
    fn test_exit_code_nonzero_exit() {
        let status = std::process::Command::new("sh")
            .arg("-c")
            .arg("exit 42")
            .status()
            .unwrap();
        assert_eq!(exit_code_from_status(status), 42);
    }

    #[test]
    fn a_teed_command_keeps_its_stderr_and_exit_status() {
        let (status, captured) = run_teeing_stderr("echo out; echo 'error: nope' >&2; exit 4");
        assert_eq!(exit_code_from_status(status.unwrap()), 4);
        let captured = captured.unwrap();
        assert_eq!(captured.stream, "stderr");
        assert_eq!(captured.text, "error: nope\n");
        assert_eq!(captured.dropped_bytes, 0);
    }

    #[test]
    fn a_background_process_holding_stderr_open_does_not_hold_up_wrap() {
        let start = std::time::Instant::now();
        let (status, captured) = run_teeing_stderr("echo first >&2; (sleep 5 >&2 &); exit 1");
        assert!(start.elapsed() < std::time::Duration::from_secs(3));
        assert_eq!(exit_code_from_status(status.unwrap()), 1);
        assert_eq!(captured.unwrap().text, "first\n");
    }

    #[cfg(unix)]
    #[test]
    fn test_exit_code_signal_kill() {
        use std::os::unix::process::ExitStatusExt;
        // Simulate a process killed by SIGKILL (signal 9) → expect 137
        let status = std::process::ExitStatus::from_raw(9); // raw signal in low byte
        assert_eq!(exit_code_from_status(status), 128 + 9);
    }

    #[cfg(unix)]
    #[test]
    fn test_exit_code_signal_segv() {
        use std::os::unix::process::ExitStatusExt;
        // Simulate SIGSEGV (signal 11) → expect 139
        let status = std::process::ExitStatus::from_raw(11);
        assert_eq!(exit_code_from_status(status), 128 + 11);
    }
}
