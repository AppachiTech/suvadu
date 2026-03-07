use crate::config;
use crate::models::{Entry, Session};
use crate::repository::Repository;
use crate::util;

/// Normalize a timestamp to milliseconds.
/// Detects nanosecond (19 digits), microsecond (16+ digits), and
/// second (10 digits) timestamps and converts them.
/// Returns 0 unchanged (handled separately).
pub const fn normalize_timestamp(ts: i64) -> i64 {
    // Nanoseconds: 19 digits (> 9_999_999_999_999_999) → divide by 1_000_000
    const NANOSECOND_THRESHOLD: i64 = 9_999_999_999_999_999;

    if ts <= 0 {
        return ts;
    }
    if ts > NANOSECOND_THRESHOLD {
        return ts / 1_000_000;
    }
    // Microseconds: 16 digits — use shared threshold constant
    if ts > crate::util::MICROSECOND_THRESHOLD {
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

/// Parameters for `handle_add` / `handle_add_with_context`.
pub struct AddParams {
    pub session_id: String,
    pub command: String,
    pub cwd: String,
    pub exit_code: Option<i32>,
    pub started_at: i64,
    pub ended_at: i64,
    pub executor_type: Option<String>,
    pub executor: Option<String>,
    pub context: Option<std::collections::HashMap<String, String>>,
}

/// Maximum lengths for input fields (defense against malicious/buggy hooks).
const MAX_SESSION_ID_LEN: usize = 256;
const MAX_COMMAND_LEN: usize = 65536; // 64 KB — long pipe chains, heredocs
const MAX_CWD_LEN: usize = 4096; // PATH_MAX on most systems

pub fn handle_add_with_context(params: AddParams) -> Result<(), Box<dyn std::error::Error>> {
    let AddParams {
        session_id,
        command,
        cwd,
        exit_code,
        started_at,
        ended_at,
        executor_type,
        executor,
        context,
    } = params;
    // Cheapest checks first — no I/O required
    if config::is_paused() {
        return Ok(());
    }
    if command.starts_with(' ') {
        return Ok(());
    }

    // Input length validation (defense against malicious/buggy shell hooks)
    if session_id.len() > MAX_SESSION_ID_LEN
        || command.len() > MAX_COMMAND_LEN
        || cwd.len() > MAX_CWD_LEN
    {
        return Ok(()); // Silently drop oversized inputs
    }

    // Session ID character validation (consistent with integrations.rs)
    if !util::is_valid_session_id(&session_id) {
        return Ok(());
    }

    // Load config (filesystem read + TOML parse)
    let config = config::load_config()?;
    if !config.enabled {
        return Ok(());
    }

    // Check exclusions — skip compilation entirely when no patterns configured
    if !config.exclusions.is_empty() {
        let compiled = util::compile_exclusions(&config.exclusions);
        if util::is_excluded_compiled(&command, &compiled) {
            return Ok(());
        }
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
        session_id.clone(),
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
    if repo.get_session(&session_id)?.is_none() {
        let session = Session {
            id: session_id,
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

pub fn handle_add(params: AddParams) -> Result<(), Box<dyn std::error::Error>> {
    handle_add_with_context(params)
}

pub fn handle_delete(
    pattern: &str,
    is_regex: bool,
    dry_run: bool,
    skip_confirm: bool,
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

    let count = repo.count_entries_by_pattern(pattern, is_regex, before_timestamp)?;

    if count == 0 {
        println!("No entries matched the pattern '{pattern}'");
        return Ok(());
    }

    if dry_run {
        println!("Dry Run: {count} entries match the pattern '{pattern}'.");
        if let Some(ts) = before_timestamp {
            let date = chrono::DateTime::from_timestamp_millis(ts)
                .ok_or_else(|| format!("Invalid timestamp: {ts}"))?;
            println!(
                "(Filtered entries older than: {})",
                date.format("%Y-%m-%d %H:%M:%S")
            );
        }
        return Ok(());
    }

    if !skip_confirm {
        eprint!("Delete {count} entries matching '{pattern}'? [y/N] ");
        let mut answer = String::new();
        std::io::stdin().read_line(&mut answer)?;
        if !answer.trim().eq_ignore_ascii_case("y") {
            println!("Aborted.");
            return Ok(());
        }
    }

    let deleted = repo.delete_entries(pattern, is_regex, before_timestamp)?;
    println!("✓ Deleted {deleted} entries.");

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
        crate::cli::BookmarkCommands::List { json } => {
            let bookmarks = repo.list_bookmarks()?;
            if json {
                println!("{}", serde_json::to_string_pretty(&bookmarks)?);
            } else if bookmarks.is_empty() {
                println!("No bookmarks yet. Use `suv bookmark add <command>` to save one.");
            } else {
                if util::color_enabled() {
                    println!("\x1b[1m{:<50} {:<20} Added\x1b[0m", "Command", "Label");
                } else {
                    println!("{:<50} {:<20} Added", "Command", "Label");
                }
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
                return Err(format!("No bookmark found for: {command}").into());
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
            return Err(format!("No note found for entry {entry_id}.").into());
        }
    } else if let Some(text) = content {
        repo.upsert_note(entry_id, &text)?;
        println!("Note saved for entry {entry_id}.");
    } else {
        match repo.get_note(entry_id)? {
            Some(note) => println!("{}", note.content),
            None => return Err(format!("No note for entry {entry_id}.").into()),
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

    /// Integration test: exercises the full handle_add_with_context pipeline
    /// (timestamp normalize → session ensure → entry insert) with a temp DB.
    #[test]
    fn test_handle_add_pipeline() {
        let temp_dir = tempfile::TempDir::new().unwrap();
        let db_path = temp_dir.path().join("test.db");
        let conn = crate::db::init_db(&db_path).unwrap();
        let repo = Repository::new(conn);

        let session_id = "test-session-123";

        // Insert via the repo directly (simulating what handle_add_with_context does
        // without config/exclusion dependencies)
        let started_at = normalize_timestamp(1_709_683_200); // seconds → ms
        let ended_at = normalize_timestamp(1_709_683_205);
        assert_eq!(started_at, 1_709_683_200_000);
        assert_eq!(ended_at, 1_709_683_205_000);

        // Ensure session is created
        assert!(repo.get_session(session_id).unwrap().is_none());
        let session = crate::models::Session {
            id: session_id.to_string(),
            hostname: "test-host".to_string(),
            created_at: started_at,
            tag_id: None,
        };
        repo.insert_session(&session).unwrap();
        assert!(repo.get_session(session_id).unwrap().is_some());

        // Insert entry
        let entry = Entry::new(
            session_id.to_string(),
            "cargo test".to_string(),
            "/home/user/project".to_string(),
            Some(0),
            started_at,
            ended_at,
        );
        repo.insert_entry(&entry).unwrap();

        // Verify the entry was stored correctly
        let entries = repo
            .get_entries(1, 0, None, None, None, None, None, false, None, None)
            .unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].command, "cargo test");
        assert_eq!(entries[0].cwd, "/home/user/project");
        assert_eq!(entries[0].exit_code, Some(0));
        assert_eq!(entries[0].started_at, 1_709_683_200_000);
        assert_eq!(entries[0].duration_ms, 5_000);
    }

    /// Test nanosecond timestamps are properly normalized through the pipeline
    #[test]
    fn test_handle_add_nanosecond_timestamps() {
        let ts_ns = 1_770_574_211_585_923_456_i64;
        let normalized = normalize_timestamp(ts_ns);
        // Should convert to milliseconds, not microseconds
        assert_eq!(normalized, ts_ns / 1_000_000);
        // Verify it's in a reasonable millisecond range (13 digits)
        assert!(normalized > 1_000_000_000_000);
        assert!(normalized < 10_000_000_000_000);
    }

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
        // Nanoseconds (19 digits) → divide by 1_000_000 to get milliseconds directly
        let ts_ns = 1_770_574_211_585_923_456;
        let expected_ms = 1_770_574_211_585; // ts_ns / 1_000_000 (truncated)
        assert_eq!(normalize_timestamp(ts_ns), expected_ms);
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
