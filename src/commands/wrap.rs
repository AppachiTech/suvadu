use std::process;

pub fn handle_wrap(
    command: &[String],
    executor_type: &str,
    executor: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    if command.is_empty() {
        eprintln!("Error: No command provided to wrap.");
        process::exit(1);
    }

    let cmd_str = command.join(" ");
    let cwd = std::env::current_dir()
        .map_or_else(|_| ".".to_string(), |p| p.to_string_lossy().to_string());

    let session_id =
        std::env::var("SUVADU_SESSION_ID").unwrap_or_else(|_| uuid::Uuid::new_v4().to_string());

    let started_at = chrono::Utc::now().timestamp_millis();

    // Execute the command
    let status = std::process::Command::new("sh")
        .arg("-c")
        .arg(&cmd_str)
        .status();

    let ended_at = chrono::Utc::now().timestamp_millis();

    let exit_code = status.map_or(127, exit_code_from_status);

    // Record in history (log failure to stderr; don't block the wrapped command's exit)
    if let Err(e) = super::entry::handle_add(
        &session_id,
        cmd_str,
        cwd,
        Some(exit_code),
        started_at,
        ended_at,
        Some(executor_type.to_string()),
        Some(executor.to_string()),
    ) {
        eprintln!("suvadu: failed to record command: {e}");
    }

    // Exit with the command's exit code
    process::exit(exit_code);
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
