use std::io::{BufRead, Write};

use crate::models::{Entry, Session};
use crate::repository::Repository;
use crate::util;

/// Escape a string for CSV: double internal quotes and prefix with `'` if the
/// field starts with a formula-triggering character (`=`, `+`, `-`, `@`, tab, CR).
/// This prevents formula injection in Excel / Google Sheets.
fn csv_safe(s: &str) -> String {
    let escaped = s.replace('"', "\"\"");
    if escaped.starts_with(['=', '+', '-', '@', '\t', '\r']) {
        format!("'{escaped}")
    } else {
        escaped
    }
}

pub fn handle_export(
    format: &str,
    after: Option<&str>,
    before: Option<&str>,
) -> Result<(), Box<dyn std::error::Error>> {
    let repo = Repository::init()?;

    let after_ms = after.and_then(|d| util::parse_date_input(d, false));
    let before_ms = before.and_then(|d| util::parse_date_input(d, true));

    match format {
        "json" => {
            // Stream JSON array: print `[`, then comma-separated entries, then `]`
            let mut count = 0usize;
            let stdout = std::io::stdout();
            let mut out = stdout.lock();
            repo.stream_export_entries(after_ms, before_ms, |entry| {
                if count == 0 {
                    writeln!(out, "[")?;
                } else {
                    writeln!(out, ",")?;
                }
                write!(out, "  {}", serde_json::to_string(&entry)?)?;
                count += 1;
                Ok(())
            })?;
            if count == 0 {
                eprintln!("No entries to export.");
            } else {
                writeln!(out, "\n]")?;
                eprintln!("Exported {count} entries.");
            }
        }
        "jsonl" => {
            let mut count = 0usize;
            repo.stream_export_entries(after_ms, before_ms, |entry| {
                println!("{}", serde_json::to_string(&entry)?);
                count += 1;
                Ok(())
            })?;
            if count == 0 {
                eprintln!("No entries to export.");
            } else {
                eprintln!("Exported {count} entries.");
            }
        }
        "csv" => {
            println!("command,cwd,exit_code,started_at,ended_at,duration_ms,session_id,executor_type,executor");
            let mut count = 0usize;
            repo.stream_export_entries(after_ms, before_ms, |entry| {
                let cmd = csv_safe(&entry.command);
                let cwd = csv_safe(&entry.cwd);
                let sid = csv_safe(&entry.session_id);
                let etype = csv_safe(entry.executor_type.as_deref().unwrap_or(""));
                let exec = csv_safe(entry.executor.as_deref().unwrap_or(""));
                println!(
                    "\"{cmd}\",\"{cwd}\",{},{},{},{},\"{sid}\",\"{etype}\",\"{exec}\"",
                    entry.exit_code.map_or(String::new(), |c| c.to_string()),
                    entry.started_at,
                    entry.ended_at,
                    entry.duration_ms,
                );
                count += 1;
                Ok(())
            })?;
            if count == 0 {
                eprintln!("No entries to export.");
            } else {
                eprintln!("Exported {count} entries.");
            }
        }
        _ => {
            return Err(format!("Unknown format: {format}. Use 'json', 'jsonl', or 'csv'.").into());
        }
    }

    Ok(())
}

pub fn handle_import(file: &str, dry_run: bool) -> Result<(), Box<dyn std::error::Error>> {
    const BATCH_SIZE: u64 = 10_000;

    let f = std::fs::File::open(file)?;
    let reader = std::io::BufReader::new(f);

    if dry_run {
        let mut count = 0u64;
        let mut skipped = 0u64;
        for (line_num, line) in reader.lines().enumerate() {
            let line = line?;
            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }
            let entry: Entry = match serde_json::from_str(trimmed) {
                Ok(e) => e,
                Err(err) => {
                    eprintln!("Line {}: parse error: {err}", line_num + 1);
                    skipped += 1;
                    continue;
                }
            };
            println!("[dry-run] Would import: {} ({})", entry.command, entry.cwd);
            count += 1;
        }
        println!(
            "Dry run complete. {count} entries would be imported ({skipped} skipped due to errors)."
        );
        return Ok(());
    }

    let repo = Repository::init()?;

    // Stream entries in batches to avoid loading entire file into memory.
    // Each batch is committed independently; a parse error skips the line,
    // an insert error rolls back only the current batch and aborts.
    let mut imported = 0u64;
    let mut parse_errors = 0u64;
    let mut batch_count = 0u64;

    repo.begin_transaction()?;

    for (line_num, line) in reader.lines().enumerate() {
        let line = line?;
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let entry: Entry = match serde_json::from_str(trimmed) {
            Ok(e) => e,
            Err(err) => {
                eprintln!("Line {}: parse error: {err}", line_num + 1);
                parse_errors += 1;
                continue;
            }
        };

        match repo.insert_entry(&entry) {
            Ok(_) => {
                imported += 1;
                batch_count += 1;
            }
            Err(e) => {
                eprintln!("Insert failed at line {}: {e}", line_num + 1);
                eprintln!("Rolling back — no entries from this batch were written.");
                let _ = repo.rollback();
                return Err(e.into());
            }
        }

        // Commit in batches to bound memory and WAL growth
        if batch_count >= BATCH_SIZE {
            repo.commit()?;
            repo.begin_transaction()?;
            batch_count = 0;
        }
    }

    repo.commit()?;
    println!("Imported {imported} entries ({parse_errors} skipped).");
    Ok(())
}

/// Parse a single extended-history line: `: timestamp:duration;command`
/// Returns (`timestamp_seconds`, `duration_seconds`, command)
pub fn parse_extended_history_line(line: &str) -> Option<(i64, i64, String)> {
    let rest = line.strip_prefix(": ")?;
    let colon_pos = rest.find(':')?;
    let ts: i64 = rest[..colon_pos].parse().ok()?;
    let after_ts = &rest[colon_pos + 1..];
    let semi_pos = after_ts.find(';')?;
    let dur: i64 = after_ts[..semi_pos].parse().ok()?;
    let cmd = after_ts[semi_pos + 1..].to_string();
    Some((ts, dur, cmd))
}

pub fn handle_import_zsh_history(
    file: &str,
    dry_run: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    // Read file with lossy UTF-8 conversion (zsh_history may contain binary data)
    let raw = std::fs::read(file)?;
    let text = String::from_utf8_lossy(&raw);
    if matches!(text, std::borrow::Cow::Owned(_)) {
        eprintln!(
            "Warning: {file} contains invalid UTF-8 bytes; those bytes were replaced with \u{FFFD}"
        );
    }

    let parsed = parse_zsh_history(&text);

    println!("Parsed {} commands from {file}", parsed.len());

    if dry_run {
        print_zsh_import_preview(&parsed);
        return Ok(());
    }

    // Phase 2: Open DB and deduplicate
    let repo = Repository::init()?;

    println!("Checking for duplicates against existing entries...");

    // Create a dedicated import session
    let session_id = format!("import-zsh-{}", uuid::Uuid::new_v4());
    let hostname = hostname::get()?.to_string_lossy().to_string();
    let now = chrono::Utc::now().timestamp_millis();

    let session = Session {
        id: session_id.clone(),
        hostname,
        created_at: now,
        tag_id: None,
    };
    repo.insert_session(&session)?;

    // Phase 3: Insert in a transaction for performance + atomicity
    repo.begin_transaction()?;

    let result = import_entries_batch(&repo, &parsed, &session_id, now);

    match result {
        Ok((imported, skipped)) => {
            repo.commit()?;
            println!("\n✓ Import complete:");
            println!("  Imported: {imported}");
            println!("  Skipped:  {skipped} (duplicates/empty)");
            println!("  Session:  {session_id}");
        }
        Err(e) => {
            eprintln!("\nImport failed: {e}");
            eprintln!("Rolling back — no entries were written.");
            let _ = repo.rollback();
            return Err(e);
        }
    }

    Ok(())
}

/// Parse zsh history text into a list of (command, `started_at_ms`, `duration_ms`) tuples.
fn parse_zsh_history(text: &str) -> Vec<(String, i64, i64)> {
    let mut parsed: Vec<(String, i64, i64)> = Vec::new();
    let mut current_cmd = String::new();
    let mut current_ts: i64 = 0;
    let mut current_dur: i64 = 0;
    let mut in_multiline = false;

    for line in text.lines() {
        if in_multiline {
            // Continuation of previous command
            current_cmd.push('\n');
            if let Some(stripped) = line.strip_suffix('\\') {
                current_cmd.push_str(stripped);
            } else {
                current_cmd.push_str(line);
                let trimmed = current_cmd.trim_end().to_string();
                parsed.push((trimmed, current_ts, current_dur));
                current_cmd.clear();
                in_multiline = false;
            }
            continue;
        }

        // Try extended history format: ": timestamp:duration;command"
        if line.starts_with(": ") {
            if let Some((ts, dur, cmd)) = parse_extended_history_line(line) {
                let ts_ms = ts * 1000;
                let dur_ms = dur * 1000;
                if let Some(stripped) = cmd.strip_suffix('\\') {
                    current_cmd = stripped.to_string();
                    current_ts = ts_ms;
                    current_dur = dur_ms;
                    in_multiline = true;
                } else {
                    parsed.push((cmd, ts_ms, dur_ms));
                }
            }
        } else if !line.trim().is_empty() {
            // Plain format (no timestamp)
            if let Some(stripped) = line.strip_suffix('\\') {
                current_cmd = stripped.to_string();
                current_ts = 0;
                current_dur = 0;
                in_multiline = true;
            } else {
                parsed.push((line.to_string(), 0, 0));
            }
        }
    }

    // Flush any remaining multiline command
    if !current_cmd.is_empty() {
        let trimmed = current_cmd.trim_end().to_string();
        parsed.push((trimmed, current_ts, current_dur));
    }

    parsed
}

/// Print a preview of parsed zsh history entries (for dry-run mode).
fn print_zsh_import_preview(parsed: &[(String, i64, i64)]) {
    println!("\nDry run — no entries written. Sample:");
    for (i, (cmd, ts, _dur)) in parsed.iter().take(10).enumerate() {
        let date = if *ts > 0 {
            chrono::DateTime::from_timestamp_millis(*ts)
                .map(|dt| {
                    dt.with_timezone(&chrono::Local)
                        .format("%Y-%m-%d %H:%M")
                        .to_string()
                })
                .unwrap_or_default()
        } else {
            "no timestamp".to_string()
        };
        let display = cmd.replace('\n', "\\n");
        let truncated = crate::util::truncate_str(&display, 60, "…");
        println!("  {:>2}. [{date}] {truncated}", i + 1);
    }
    if parsed.len() > 10 {
        println!("  ... and {} more", parsed.len() - 10);
    }
}

/// Insert parsed entries in a batch. Returns (imported, skipped) counts.
/// Errors are fatal — the caller is responsible for rolling back the transaction.
fn import_entries_batch(
    repo: &Repository,
    parsed: &[(String, i64, i64)],
    session_id: &str,
    now: i64,
) -> Result<(u64, u64), Box<dyn std::error::Error>> {
    let mut imported = 0u64;
    let mut skipped = 0u64;
    let total = parsed.len();

    for (i, (cmd, ts, dur)) in parsed.iter().enumerate() {
        // Skip empty or space-prefixed commands
        if cmd.trim().is_empty() || cmd.starts_with(' ') {
            skipped += 1;
            continue;
        }

        // Dedup: skip if (command, timestamp_ms) already exists.
        // Uses indexed SQL lookup instead of loading all entries into memory.
        if *ts > 0 && repo.entry_exists(cmd, *ts)? {
            skipped += 1;
            continue;
        }

        let started_at = if *ts > 0 { *ts } else { now };
        let ended_at = started_at + dur;

        let entry = Entry::new(
            session_id.to_string(),
            cmd.clone(),
            String::new(), // CWD unknown for imported entries
            None,          // exit code unknown
            started_at,
            ended_at,
        );

        repo.insert_entry(&entry)?;
        imported += 1;

        // Progress every 2000 entries
        if (i + 1) % 2000 == 0 {
            eprint!("\r  Progress: {}/{total}...", i + 1);
        }
    }

    if total >= 2000 {
        eprintln!(); // Clear progress line
    }

    Ok((imported, skipped))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_extended_history_line() {
        // Standard extended history format
        let (ts, dur, cmd) = parse_extended_history_line(": 1724827236:0;git status").unwrap();
        assert_eq!(ts, 1_724_827_236);
        assert_eq!(dur, 0);
        assert_eq!(cmd, "git status");
    }

    #[test]
    fn test_parse_extended_history_with_duration() {
        let (ts, dur, cmd) =
            parse_extended_history_line(": 1724827300:15;cargo build --release").unwrap();
        assert_eq!(ts, 1_724_827_300);
        assert_eq!(dur, 15);
        assert_eq!(cmd, "cargo build --release");
    }

    #[test]
    fn test_parse_extended_history_with_semicolons_in_command() {
        // Command itself contains semicolons
        let (ts, dur, cmd) =
            parse_extended_history_line(": 1724827236:0;echo hello; echo world").unwrap();
        assert_eq!(ts, 1_724_827_236);
        assert_eq!(dur, 0);
        assert_eq!(cmd, "echo hello; echo world");
    }

    #[test]
    fn test_parse_extended_history_invalid() {
        assert!(parse_extended_history_line("not a history line").is_none());
        assert!(parse_extended_history_line(": abc:0;cmd").is_none());
        assert!(parse_extended_history_line(": 123").is_none());
    }

    #[test]
    fn test_parse_extended_history_empty_command() {
        // Empty command after semicolon: `: 123:0;`
        let result = parse_extended_history_line(": 123:0;");
        assert!(result.is_some(), "Should parse even with empty command");
        let (ts, dur, cmd) = result.unwrap();
        assert_eq!(ts, 123);
        assert_eq!(dur, 0);
        assert_eq!(cmd, "", "Command should be empty string");
    }

    #[test]
    fn test_parse_extended_history_multiline_marker() {
        // Lines that start with continuation (backslash at end) are handled by
        // the multiline logic in handle_import_zsh_history, not by parse_extended_history_line.
        // But parse_extended_history_line should still correctly parse a command ending with backslash.
        let result = parse_extended_history_line(": 1724827236:0;echo hello \\");
        assert!(result.is_some());
        let (_ts, _dur, cmd) = result.unwrap();
        // The raw line parser just returns the command as-is, including the trailing backslash
        assert!(
            cmd.ends_with('\\'),
            "Command should preserve trailing backslash: {cmd}"
        );
    }
}
