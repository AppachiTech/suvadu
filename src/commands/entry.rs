use crate::config;
use crate::models::{Entry, Session};
use crate::repository::Repository;
use crate::util;

/// Normalize a timestamp to milliseconds.
/// Detects microsecond timestamps (16+ digits) and converts them.
/// Detects second timestamps (10 digits) and converts them.
/// Returns 0 unchanged (handled separately).
pub const fn normalize_timestamp(ts: i64) -> i64 {
    if ts <= 0 {
        return ts;
    }
    // Microseconds: 16 digits (> 9_999_999_999_999 i.e. > ~Nov 2286 in ms)
    if ts > 9_999_999_999_999 {
        return ts / 1000;
    }
    // Seconds: 10 digits (< 100_000_000_000 i.e. < ~1973 in ms)
    // Current epoch seconds are ~1.7 billion (10 digits), ms would be 13 digits
    if ts < 100_000_000_000 {
        return ts * 1000;
    }
    // Already milliseconds
    ts
}

#[allow(clippy::too_many_arguments)]
pub fn handle_add_with_context(
    session_id: &str,
    command: String,
    cwd: String,
    exit_code: Option<i32>,
    started_at: i64,
    ended_at: i64,
    executor_type: Option<String>,
    executor: Option<String>,
    context: Option<std::collections::HashMap<String, String>>,
) -> Result<(), Box<dyn std::error::Error>> {
    // Check config
    let config = config::load_config()?;
    if !config.enabled || config::is_paused() {
        return Ok(());
    }

    // Sanitize: ignore commands starting with space (privacy feature)
    if command.starts_with(' ') {
        return Ok(()); // Silently skip
    }

    // Check exclusions (pre-compiled for performance on hot path)
    let compiled = util::compile_exclusions(&config.exclusions);
    if util::is_excluded_compiled(&command, &compiled) {
        return Ok(());
    }

    // Initialize database
    let repo = Repository::init()?;

    // Auto-Tagging Logic (Path-based)
    let mut matched_tag_id: Option<i64> = None;
    if !config.auto_tags.is_empty() {
        if let Some(tag_name) = util::resolve_auto_tag(&cwd, &config.auto_tags) {
            if let Some(id) = repo.get_tag_id_by_name(&tag_name)? {
                matched_tag_id = Some(id);
            } else {
                // Auto-create tag if configured in config but missing in DB
                match repo.create_tag(&tag_name, Some("Auto-created from path config")) {
                    Ok(id) => matched_tag_id = Some(id),
                    Err(e) => eprintln!("suvadu: failed to auto-create tag '{tag_name}': {e}"),
                }
            }
        }
    }

    // Normalize timestamps to milliseconds (guards against micro/nanosecond inputs)
    let started_at = normalize_timestamp(started_at);
    let ended_at = normalize_timestamp(ended_at);

    // If started_at is still 0, use ended_at; if both 0, use current time
    let started_at = if started_at == 0 {
        if ended_at > 0 {
            ended_at
        } else {
            chrono::Utc::now().timestamp_millis()
        }
    } else {
        started_at
    };
    let ended_at = if ended_at == 0 { started_at } else { ended_at };

    // Create entry
    let mut entry = Entry::new(
        session_id.to_string(),
        command,
        cwd,
        exit_code,
        started_at,
        ended_at,
    )
    .with_tag_id(matched_tag_id);

    // Set executor information
    entry.executor_type = executor_type;
    entry.executor = executor;
    entry.context = context;

    // Ensure session exists
    if repo.get_session(session_id)?.is_none() {
        let session = Session {
            id: session_id.to_string(),
            hostname: hostname::get()?.to_string_lossy().to_string(),
            created_at: started_at,
            tag_id: None,
        };
        repo.insert_session(&session)?;
    }

    // Insert entry
    repo.insert_entry(&entry)?;

    Ok(()) // Silent success
}

#[allow(clippy::too_many_arguments)]
pub fn handle_add(
    session_id: &str,
    command: String,
    cwd: String,
    exit_code: Option<i32>,
    started_at: i64,
    ended_at: i64,
    executor_type: Option<String>,
    executor: Option<String>,
) -> Result<(), Box<dyn std::error::Error>> {
    handle_add_with_context(
        session_id,
        command,
        cwd,
        exit_code,
        started_at,
        ended_at,
        executor_type,
        executor,
        None,
    )
}

pub fn handle_delete(
    pattern: &str,
    is_regex: bool,
    dry_run: bool,
    before: Option<&str>,
) -> Result<(), Box<dyn std::error::Error>> {
    if pattern.is_empty() {
        return Err(
            "Empty pattern would match all entries. Please provide a specific pattern.".into(),
        );
    }

    let repo = Repository::init()?;

    let before_timestamp: Option<i64> = if let Some(date_str) = before {
        Some(util::parse_date_input(date_str, false).ok_or_else(|| {
            format!("Invalid date format: {date_str}. Use YYYY-MM-DD or keywords.")
        })?)
    } else {
        None
    };

    if dry_run {
        let count = repo.count_entries_by_pattern(pattern, is_regex, before_timestamp)?;
        if count == 0 {
            println!("No entries matched the pattern '{pattern}'");
        } else {
            println!("Dry Run: {count} entries match the pattern '{pattern}'.");
            if let Some(ts) = before_timestamp {
                let date = chrono::DateTime::from_timestamp_millis(ts)
                    .ok_or_else(|| format!("Invalid timestamp: {ts}"))?;
                println!(
                    "(Filtered entries older than: {})",
                    date.format("%Y-%m-%d %H:%M:%S")
                );
            }
        }
    } else {
        println!("Deleting entries matching pattern '{pattern}'...");
        if let Some(ts) = before_timestamp {
            let date = chrono::DateTime::from_timestamp_millis(ts)
                .ok_or_else(|| format!("Invalid timestamp: {ts}"))?;
            println!("  and older than: {}", date.format("%Y-%m-%d %H:%M:%S"));
        }
        let deleted = repo.delete_entries(pattern, is_regex, before_timestamp)?;
        println!("✓ Deleted {deleted} entries.");
    }

    Ok(())
}

pub fn handle_bookmark(
    cmd: crate::cli::BookmarkCommands,
) -> Result<(), Box<dyn std::error::Error>> {
    let repo = Repository::init()?;

    match cmd {
        crate::cli::BookmarkCommands::Add { command, label } => {
            repo.add_bookmark(&command, label.as_deref())?;
            println!("Bookmarked: {command}");
        }
        crate::cli::BookmarkCommands::List => {
            let bookmarks = repo.list_bookmarks()?;
            if bookmarks.is_empty() {
                println!("No bookmarks yet. Use `suv bookmark add <command>` to save one.");
            } else {
                println!("\x1b[1m{:<50} {:<20} Added\x1b[0m", "Command", "Label");
                for bm in &bookmarks {
                    let date = chrono::DateTime::from_timestamp_millis(bm.created_at)
                        .map(|dt| dt.format("%Y-%m-%d").to_string())
                        .unwrap_or_default();
                    let label_str = bm.label.as_deref().unwrap_or("-");
                    let cmd_display = crate::util::truncate_str(&bm.command, 48, "…");
                    println!("{cmd_display:<50} {label_str:<20} {date}");
                }
                println!("\n{} bookmark(s)", bookmarks.len());
            }
        }
        crate::cli::BookmarkCommands::Remove { command } => {
            if repo.remove_bookmark(&command)? {
                println!("Removed bookmark: {command}");
            } else {
                eprintln!("No bookmark found for: {command}");
            }
        }
    }
    Ok(())
}

pub fn handle_note(
    entry_id: i64,
    content: Option<String>,
    delete: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    let repo = Repository::init()?;

    if delete {
        if repo.delete_note(entry_id)? {
            println!("Note deleted for entry {entry_id}.");
        } else {
            eprintln!("No note found for entry {entry_id}.");
        }
    } else if let Some(text) = content {
        repo.upsert_note(entry_id, &text)?;
        println!("Note saved for entry {entry_id}.");
    } else {
        match repo.get_note(entry_id)? {
            Some(note) => println!("{}", note.content),
            None => eprintln!("No note for entry {entry_id}."),
        }
    }
    Ok(())
}

pub fn handle_gc(dry_run: bool, vacuum: bool) -> Result<(), Box<dyn std::error::Error>> {
    let repo = Repository::init()?;

    let orphaned_sessions = repo.count_orphaned_sessions()?;
    let orphaned_notes = repo.count_orphaned_notes()?;
    let stale_prompts = count_stale_prompt_caches();

    if dry_run {
        println!("Dry run — nothing will be deleted.\n");
        println!("  Orphaned sessions (no entries): {orphaned_sessions}");
        println!("  Orphaned notes (missing entry): {orphaned_notes}");
        println!("  Stale prompt cache files:       {stale_prompts}");
        if orphaned_sessions == 0 && orphaned_notes == 0 && stale_prompts == 0 {
            println!("\nNothing to clean up.");
        }
        return Ok(());
    }

    let deleted_notes = repo.delete_orphaned_notes()?;
    let deleted_sessions = repo.delete_orphaned_sessions()?;
    let deleted_prompts = clean_prompt_caches();

    if deleted_sessions > 0 {
        println!("Removed {deleted_sessions} orphaned sessions.");
    }
    if deleted_notes > 0 {
        println!("Removed {deleted_notes} orphaned notes.");
    }
    if deleted_prompts > 0 {
        println!("Removed {deleted_prompts} stale prompt cache files.");
    }
    if deleted_sessions == 0 && deleted_notes == 0 && deleted_prompts == 0 {
        println!("Nothing to clean up.");
    }

    if vacuum {
        println!("Running VACUUM...");
        repo.vacuum()?;
        println!("Database compacted.");
    }

    Ok(())
}

/// Count prompt cache files older than 7 days.
fn count_stale_prompt_caches() -> u64 {
    let Some(prompts_dir) = get_prompts_dir() else {
        return 0;
    };
    count_old_files(&prompts_dir, 7 * 24 * 3600)
}

/// Delete prompt cache files older than 7 days. Returns count deleted.
fn clean_prompt_caches() -> u64 {
    let Some(prompts_dir) = get_prompts_dir() else {
        return 0;
    };
    delete_old_files(&prompts_dir, 7 * 24 * 3600)
}

fn get_prompts_dir() -> Option<std::path::PathBuf> {
    let home = std::env::var("HOME").ok()?;
    Some(
        std::path::PathBuf::from(home)
            .join(".config")
            .join("suvadu")
            .join("prompts"),
    )
}

fn count_old_files(dir: &std::path::Path, max_age_secs: u64) -> u64 {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return 0;
    };
    let now = std::time::SystemTime::now();
    let mut count = 0u64;
    for entry in entries.flatten() {
        if let Ok(meta) = entry.metadata() {
            if meta.is_file() {
                if let Ok(modified) = meta.modified() {
                    if let Ok(age) = now.duration_since(modified) {
                        if age.as_secs() > max_age_secs {
                            count += 1;
                        }
                    }
                }
            }
        }
    }
    count
}

fn delete_old_files(dir: &std::path::Path, max_age_secs: u64) -> u64 {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return 0;
    };
    let now = std::time::SystemTime::now();
    let mut deleted = 0u64;
    for entry in entries.flatten() {
        if let Ok(meta) = entry.metadata() {
            if meta.is_file() {
                if let Ok(modified) = meta.modified() {
                    if let Ok(age) = now.duration_since(modified) {
                        if age.as_secs() > max_age_secs
                            && std::fs::remove_file(entry.path()).is_ok()
                        {
                            deleted += 1;
                        }
                    }
                }
            }
        }
    }
    deleted
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_normalize_timestamp_milliseconds() {
        // Already milliseconds (13 digits) — no change
        let ts = 1_770_693_885_695;
        assert_eq!(normalize_timestamp(ts), ts);
    }

    #[test]
    fn test_normalize_timestamp_microseconds() {
        // Microseconds (16 digits) → divide by 1000
        let ts_us = 1_770_574_211_585_923;
        let ts_ms = ts_us / 1000;
        assert_eq!(normalize_timestamp(ts_us), ts_ms);
    }

    #[test]
    fn test_normalize_timestamp_seconds() {
        // Seconds (10 digits) → multiply by 1000
        let ts_s = 1_770_693_885;
        assert_eq!(normalize_timestamp(ts_s), ts_s * 1000);
    }

    #[test]
    fn test_normalize_timestamp_zero() {
        assert_eq!(normalize_timestamp(0), 0);
    }

    #[test]
    fn test_normalize_timestamp_negative() {
        // Negative values are returned as-is
        assert_eq!(normalize_timestamp(-1), -1);
        assert_eq!(normalize_timestamp(-1000), -1000);
    }

    #[test]
    fn test_normalize_timestamp_boundary_seconds_ms() {
        // 99_999_999_999 is seconds (10 digits) → multiply by 1000
        assert_eq!(normalize_timestamp(99_999_999_999), 99_999_999_999 * 1000);
        // 100_000_000_000 is milliseconds (12 digits) → no change
        assert_eq!(normalize_timestamp(100_000_000_000), 100_000_000_000);
    }

    #[test]
    fn test_normalize_timestamp_boundary_ms_us() {
        // 9_999_999_999_999 is milliseconds → no change
        assert_eq!(normalize_timestamp(9_999_999_999_999), 9_999_999_999_999);
        // 10_000_000_000_000 is microseconds → divide by 1000
        assert_eq!(normalize_timestamp(10_000_000_000_000), 10_000_000_000);
    }

    #[test]
    fn test_normalize_timestamp_nanoseconds() {
        // Nanoseconds (19 digits) → divide by 1000 (becomes microseconds)
        // This is a known limitation — only one level of conversion
        let ts_ns = 1_770_574_211_585_923_456;
        assert_eq!(normalize_timestamp(ts_ns), ts_ns / 1000);
    }

    #[test]
    fn test_normalize_timestamp_current_epoch() {
        // Current epoch in seconds (~1.7 billion)
        let ts_s = 1_709_683_200; // 2024-03-06 in seconds
        assert_eq!(normalize_timestamp(ts_s), ts_s * 1000);

        // Same in milliseconds
        let ts_ms = 1_709_683_200_000;
        assert_eq!(normalize_timestamp(ts_ms), ts_ms);
    }
}
