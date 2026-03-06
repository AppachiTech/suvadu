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

    let exit_code = status.as_ref().map_or(127, |s| s.code().unwrap_or(1));

    // Record in history
    let _ = super::entry::handle_add(
        &session_id,
        cmd_str,
        cwd,
        Some(exit_code),
        started_at,
        ended_at,
        Some(executor_type.to_string()),
        Some(executor.to_string()),
    );

    // Exit with the command's exit code
    process::exit(exit_code);
}
