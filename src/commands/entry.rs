use std::sync::{Arc, Mutex};

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

// ── Exclusion pattern cache ─────────────────────────────────
// Compiled regex exclusions are cached and only recompiled when the
// source patterns change (i.e. when the config file is edited).

struct CachedExclusions {
    source_patterns: Vec<String>,
    compiled: Arc<Vec<util::CompiledExclusion>>,
}

static EXCLUSION_CACHE: Mutex<Option<CachedExclusions>> = Mutex::new(None);

/// Return compiled exclusion patterns, reusing the cache when patterns are unchanged.
fn get_compiled_exclusions(patterns: &[String]) -> Arc<Vec<util::CompiledExclusion>> {
    if let Ok(guard) = EXCLUSION_CACHE.lock() {
        if let Some(cached) = guard.as_ref() {
            if cached.source_patterns == *patterns {
                return Arc::clone(&cached.compiled);
            }
        }
    }

    let compiled = Arc::new(util::compile_exclusions(patterns));

    if let Ok(mut guard) = EXCLUSION_CACHE.lock() {
        *guard = Some(CachedExclusions {
            source_patterns: patterns.to_vec(),
            compiled: Arc::clone(&compiled),
        });
    }

    compiled
}

pub fn handle_add_with_context(params: AddParams) -> Result<(), Box<dyn std::error::Error>> {
    // Cheapest checks first — no I/O required
    if config::is_paused() {
        return Ok(());
    }
    if should_drop_early(&params) {
        return Ok(());
    }

    // Load config (cached; re-reads only when file mtime changes)
    let config = config::load_config_cached()?;

    // Initialize database
    let repo = Repository::init()?;

    handle_add_inner(params, &config, &repo)
}

/// Pre-check: returns `true` if the input should be silently dropped
/// (space-prefixed, oversized, or invalid session ID).
fn should_drop_early(params: &AddParams) -> bool {
    if params.command.starts_with(' ') {
        return true;
    }
    if params.session_id.len() > MAX_SESSION_ID_LEN
        || params.command.len() > MAX_COMMAND_LEN
        || params.cwd.len() > MAX_CWD_LEN
    {
        return true;
    }
    if !util::is_valid_session_id(&params.session_id) {
        return true;
    }
    false
}

/// Core ingestion logic, separated from global state for testability.
/// Handles: config-enabled check, exclusions, auto-tagging, timestamp
/// normalization, redaction, session creation, and entry insertion.
fn handle_add_inner(
    params: AddParams,
    config: &config::Config,
    repo: &Repository,
) -> Result<(), Box<dyn std::error::Error>> {
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

    if !config.enabled {
        return Ok(());
    }

    // Check exclusions — cached compiled patterns, only recompiled when config changes
    if !config.exclusions.is_empty() {
        let compiled = get_compiled_exclusions(&config.exclusions);
        if util::is_excluded_compiled(&command, &compiled) {
            return Ok(());
        }
    }

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

    // Redact secrets before storage (unless disabled in config)
    let command = if config.redaction.enabled {
        crate::redact::redact_secrets(&command)
    } else {
        command
    };

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

pub fn handle_delete(
    pattern: &str,
    is_regex: bool,
    dry_run: bool,
    skip_confirm: bool,
    before: Option<&str>,
) -> Result<(), Box<dyn std::error::Error>> {
    let repo = Repository::init()?;
    handle_delete_with_repo(&repo, pattern, is_regex, dry_run, skip_confirm, before)
}

fn handle_delete_with_repo(
    repo: &Repository,
    pattern: &str,
    is_regex: bool,
    dry_run: bool,
    skip_confirm: bool,
    before: Option<&str>,
) -> Result<(), Box<dyn std::error::Error>> {
    if pattern.trim().is_empty() {
        return Err(
            "Empty pattern would match all entries. Please provide a specific pattern.".into(),
        );
    }

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
    handle_bookmark_with_repo(&repo, cmd)
}

fn handle_bookmark_with_repo(
    repo: &Repository,
    cmd: crate::cli::BookmarkCommands,
) -> Result<(), Box<dyn std::error::Error>> {
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
    handle_note_with_repo(&repo, entry_id, content, delete)
}

fn handle_note_with_repo(
    repo: &Repository,
    entry_id: i64,
    content: Option<String>,
    delete: bool,
) -> Result<(), Box<dyn std::error::Error>> {
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

    let stale_prompts = count_stale_prompt_caches();

    if dry_run {
        let (orphaned_sessions, orphaned_notes) = count_gc_candidates(&repo)?;
        println!("Dry run — nothing will be deleted.\n");
        println!("  Orphaned sessions (no entries): {orphaned_sessions}");
        println!("  Orphaned notes (missing entry): {orphaned_notes}");
        println!("  Stale prompt cache files:       {stale_prompts}");
        if orphaned_sessions == 0 && orphaned_notes == 0 && stale_prompts == 0 {
            println!("\nNothing to clean up.");
        }
        return Ok(());
    }

    let (deleted_sessions, deleted_notes) = handle_gc_with_repo(&repo, false, vacuum)?;
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

    Ok(())
}

/// Count repo-level GC candidates (orphaned sessions and notes).
fn count_gc_candidates(repo: &Repository) -> Result<(i64, i64), Box<dyn std::error::Error>> {
    let orphaned_sessions = repo.count_orphaned_sessions()?;
    let orphaned_notes = repo.count_orphaned_notes()?;
    Ok((orphaned_sessions, orphaned_notes))
}

/// Perform repo-level garbage collection. Returns `(deleted_sessions, deleted_notes)`.
fn handle_gc_with_repo(
    repo: &Repository,
    dry_run: bool,
    vacuum: bool,
) -> Result<(usize, usize), Box<dyn std::error::Error>> {
    if dry_run {
        let orphaned_sessions = repo.count_orphaned_sessions()?;
        let orphaned_notes = repo.count_orphaned_notes()?;
        println!("Dry run — repo cleanup preview:");
        println!("  Orphaned sessions (no entries): {orphaned_sessions}");
        println!("  Orphaned notes (missing entry): {orphaned_notes}");
        return Ok((0, 0));
    }

    let deleted_notes = repo.delete_orphaned_notes()?;
    let deleted_sessions = repo.delete_orphaned_sessions()?;

    if vacuum {
        println!("Running VACUUM...");
        repo.vacuum()?;
        println!("Database compacted.");
    }

    Ok((deleted_sessions, deleted_notes))
}

/// Maximum age for prompt cache files (7 days).
const PROMPT_CACHE_MAX_AGE_SECS: u64 = 7 * 24 * 3600;

/// Count prompt cache files older than the threshold.
fn count_stale_prompt_caches() -> u64 {
    let Some(prompts_dir) = get_prompts_dir() else {
        return 0;
    };
    process_old_files(&prompts_dir, PROMPT_CACHE_MAX_AGE_SECS, false)
}

/// Delete prompt cache files older than the threshold. Returns count deleted.
fn clean_prompt_caches() -> u64 {
    let Some(prompts_dir) = get_prompts_dir() else {
        return 0;
    };
    process_old_files(&prompts_dir, PROMPT_CACHE_MAX_AGE_SECS, true)
}

fn get_prompts_dir() -> Option<std::path::PathBuf> {
    let dirs = crate::util::project_dirs()?;
    Some(dirs.data_dir().join("prompts"))
}

/// Count (and optionally delete) files older than `max_age_secs`.
fn process_old_files(dir: &std::path::Path, max_age_secs: u64, delete: bool) -> u64 {
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
                            if delete {
                                if std::fs::remove_file(entry.path()).is_ok() {
                                    count += 1;
                                }
                            } else {
                                count += 1;
                            }
                        }
                    }
                }
            }
        }
    }
    count
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
            .get_entries_filtered(1, 0, &crate::repository::QueryFilter::default())
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

    #[test]
    fn test_exclusion_cache_reuses_compiled() {
        let patterns = vec!["^ls$".to_string(), "password".to_string()];

        // First call compiles and caches
        let compiled1 = get_compiled_exclusions(&patterns);
        assert_eq!(compiled1.len(), 2);

        // Second call with same patterns returns cached version (same Arc)
        let compiled2 = get_compiled_exclusions(&patterns);
        assert!(Arc::ptr_eq(&compiled1, &compiled2));

        // Different patterns trigger recompilation
        let new_patterns = vec!["^cd".to_string()];
        let compiled3 = get_compiled_exclusions(&new_patterns);
        assert_eq!(compiled3.len(), 1);
        assert!(!Arc::ptr_eq(&compiled1, &compiled3));
    }

    #[test]
    fn test_exclusion_cache_empty_patterns() {
        let empty: Vec<String> = vec![];
        let compiled = get_compiled_exclusions(&empty);
        assert!(compiled.is_empty());
    }

    // ── Test helpers ────────────────────────────────────────────

    use crate::cli::BookmarkCommands;
    use crate::test_utils::test_repo;

    /// Seed a session and a set of command entries, returning the entry IDs.
    fn seed_entries(repo: &Repository, commands: &[&str]) -> Vec<i64> {
        let session_id = "test-session";
        let session = Session {
            id: session_id.to_string(),
            hostname: "test-host".to_string(),
            created_at: 1_700_000_000_000,
            tag_id: None,
        };
        repo.insert_session(&session).unwrap();

        let mut ids = Vec::new();
        for (i, cmd) in commands.iter().enumerate() {
            let ts = 1_700_000_000_000 + (i as i64 * 1000);
            let entry = Entry::new(
                session_id.to_string(),
                cmd.to_string(),
                "/tmp".to_string(),
                Some(0),
                ts,
                ts + 100,
            );
            let id = repo.insert_entry(&entry).unwrap();
            ids.push(id);
        }
        ids
    }

    // ── handle_delete tests ─────────────────────────────────────

    #[test]
    fn test_handle_delete_dry_run() {
        let (_dir, repo) = test_repo();
        seed_entries(&repo, &["git status", "git commit", "cargo build"]);

        // dry_run should not delete anything
        handle_delete_with_repo(&repo, "git", false, true, true, None).unwrap();

        // Verify all entries still exist
        let entries = repo
            .get_entries_filtered(100, 0, &crate::repository::QueryFilter::default())
            .unwrap();
        assert_eq!(entries.len(), 3);
    }

    #[test]
    fn test_handle_delete_no_match() {
        let (_dir, repo) = test_repo();
        seed_entries(&repo, &["git status", "cargo build"]);

        let result =
            handle_delete_with_repo(&repo, "nonexistent_pattern", false, false, true, None);
        assert!(result.is_ok());

        let entries = repo
            .get_entries_filtered(100, 0, &crate::repository::QueryFilter::default())
            .unwrap();
        assert_eq!(entries.len(), 2);
    }

    #[test]
    fn test_handle_delete_regex_pattern() {
        let (_dir, repo) = test_repo();
        seed_entries(&repo, &["git status", "git commit", "cargo build"]);

        // Delete entries matching "^git" regex, skip confirmation
        handle_delete_with_repo(&repo, "^git", true, false, true, None).unwrap();

        let entries = repo
            .get_entries_filtered(100, 0, &crate::repository::QueryFilter::default())
            .unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].command, "cargo build");
    }

    #[test]
    fn test_handle_delete_empty_pattern_error() {
        let (_dir, repo) = test_repo();
        let result = handle_delete_with_repo(&repo, "", false, false, true, None);
        assert!(result.is_err());
    }

    // ── handle_bookmark tests ───────────────────────────────────

    #[test]
    fn test_handle_bookmark_add_and_list() {
        let (_dir, repo) = test_repo();
        repo.add_bookmark("git status", None).unwrap();
        let bookmarks = repo.list_bookmarks().unwrap();
        assert_eq!(bookmarks.len(), 1);
        assert_eq!(bookmarks[0].command, "git status");
    }

    #[test]
    fn test_handle_bookmark_remove() {
        let (_dir, repo) = test_repo();
        repo.add_bookmark("git status", None).unwrap();
        assert!(repo.remove_bookmark("git status").unwrap());
        assert!(repo.list_bookmarks().unwrap().is_empty());
    }

    #[test]
    fn test_handle_bookmark_remove_nonexistent() {
        let (_dir, repo) = test_repo();
        assert!(!repo.remove_bookmark("nonexistent").unwrap());
    }

    #[test]
    fn test_handle_bookmark_with_repo_add() {
        let (_dir, repo) = test_repo();
        handle_bookmark_with_repo(
            &repo,
            BookmarkCommands::Add {
                command: "cargo test".to_string(),
                label: Some("run tests".to_string()),
            },
        )
        .unwrap();
        let bookmarks = repo.list_bookmarks().unwrap();
        assert_eq!(bookmarks.len(), 1);
        assert_eq!(bookmarks[0].command, "cargo test");
        assert_eq!(bookmarks[0].label.as_deref(), Some("run tests"));
    }

    #[test]
    fn test_handle_bookmark_with_repo_list() {
        let (_dir, repo) = test_repo();
        repo.add_bookmark("git status", None).unwrap();
        // Should not error
        handle_bookmark_with_repo(&repo, BookmarkCommands::List { json: false }).unwrap();
        handle_bookmark_with_repo(&repo, BookmarkCommands::List { json: true }).unwrap();
    }

    #[test]
    fn test_handle_bookmark_with_repo_remove() {
        let (_dir, repo) = test_repo();
        repo.add_bookmark("git status", None).unwrap();
        handle_bookmark_with_repo(
            &repo,
            BookmarkCommands::Remove {
                command: "git status".to_string(),
            },
        )
        .unwrap();
        assert!(repo.list_bookmarks().unwrap().is_empty());
    }

    #[test]
    fn test_handle_bookmark_with_repo_remove_nonexistent_errors() {
        let (_dir, repo) = test_repo();
        let result = handle_bookmark_with_repo(
            &repo,
            BookmarkCommands::Remove {
                command: "nonexistent".to_string(),
            },
        );
        assert!(result.is_err());
    }

    // ── handle_note tests ───────────────────────────────────────

    #[test]
    fn test_handle_note_upsert_and_read() {
        let (_dir, repo) = test_repo();
        let ids = seed_entries(&repo, &["git status"]);
        let entry_id = ids[0];

        // Upsert a note
        handle_note_with_repo(
            &repo,
            entry_id,
            Some("important command".to_string()),
            false,
        )
        .unwrap();

        // Read it back via repo
        let note = repo.get_note(entry_id).unwrap().unwrap();
        assert_eq!(note.content, "important command");

        // Read it back via handler (should print, not error)
        handle_note_with_repo(&repo, entry_id, None, false).unwrap();
    }

    #[test]
    fn test_handle_note_delete() {
        let (_dir, repo) = test_repo();
        let ids = seed_entries(&repo, &["git status"]);
        let entry_id = ids[0];

        // Add and then delete
        repo.upsert_note(entry_id, "a note").unwrap();
        handle_note_with_repo(&repo, entry_id, None, true).unwrap();

        // Verify note is gone
        assert!(repo.get_note(entry_id).unwrap().is_none());
    }

    #[test]
    fn test_handle_note_read_nonexistent() {
        let (_dir, repo) = test_repo();
        // Read note for non-existent entry_id
        let result = handle_note_with_repo(&repo, 9999, None, false);
        assert!(result.is_err());
    }

    #[test]
    fn test_handle_note_delete_nonexistent() {
        let (_dir, repo) = test_repo();
        let result = handle_note_with_repo(&repo, 9999, None, true);
        assert!(result.is_err());
    }

    // ── handle_gc tests ─────────────────────────────────────────

    #[test]
    fn test_handle_gc_dry_run_no_orphans() {
        let (_dir, repo) = test_repo();
        seed_entries(&repo, &["git status"]);
        // dry_run should succeed and not delete anything
        let (sessions, notes) = handle_gc_with_repo(&repo, true, false).unwrap();
        assert_eq!(sessions, 0);
        assert_eq!(notes, 0);
    }

    #[test]
    fn test_handle_gc_cleans_orphan_sessions() {
        let (_dir, repo) = test_repo();
        // Insert an orphaned session (no entries)
        let orphan = Session {
            id: "orphan-session".to_string(),
            hostname: "test-host".to_string(),
            created_at: 1_700_000_000_000,
            tag_id: None,
        };
        repo.insert_session(&orphan).unwrap();

        // Verify session exists
        assert!(repo.get_session("orphan-session").unwrap().is_some());

        let (deleted_sessions, _) = handle_gc_with_repo(&repo, false, false).unwrap();
        assert_eq!(deleted_sessions, 1);

        // Session should be gone now
        assert!(repo.get_session("orphan-session").unwrap().is_none());
    }

    #[test]
    fn test_handle_gc_cleans_orphan_notes() {
        // CASCADE means normal deletion cascades notes too,
        // so we simulate an orphan by inserting a note with FK checks off.
        let dir = tempfile::TempDir::new().unwrap();
        let db_path = dir.path().join("test.db");
        let conn = crate::db::init_db(&db_path).unwrap();
        let repo = Repository::new(conn);

        seed_entries(&repo, &["git status"]);

        // Open a second connection with FK off to insert an orphaned note
        {
            let raw = rusqlite::Connection::open(&db_path).unwrap();
            raw.pragma_update(None, "foreign_keys", "OFF").unwrap();
            raw.execute(
                "INSERT INTO notes (entry_id, note, created_at, updated_at) VALUES (9999, 'orphan', 0, 0)",
                [],
            )
            .unwrap();
        }

        // Verify the orphan is detected
        assert_eq!(repo.count_orphaned_notes().unwrap(), 1);

        let (_, deleted_notes) = handle_gc_with_repo(&repo, false, false).unwrap();
        assert_eq!(deleted_notes, 1);
    }

    #[test]
    fn test_handle_gc_vacuum() {
        let (_dir, repo) = test_repo();
        // Just verify vacuum doesn't error
        handle_gc_with_repo(&repo, false, true).unwrap();
    }

    // ── should_drop_early tests ───────────────────────────────

    fn make_params(command: &str) -> AddParams {
        AddParams {
            session_id: "test-session-abc".to_string(),
            command: command.to_string(),
            cwd: "/tmp".to_string(),
            exit_code: Some(0),
            started_at: 1_709_683_200_000,
            ended_at: 1_709_683_205_000,
            executor_type: None,
            executor: None,
            context: None,
        }
    }

    #[test]
    fn test_drop_early_space_prefixed_command() {
        let params = make_params(" secret-command");
        assert!(should_drop_early(&params));
    }

    #[test]
    fn test_drop_early_normal_command_passes() {
        let params = make_params("git status");
        assert!(!should_drop_early(&params));
    }

    #[test]
    fn test_drop_early_oversized_session_id() {
        let mut params = make_params("ls");
        params.session_id = "x".repeat(MAX_SESSION_ID_LEN + 1);
        assert!(should_drop_early(&params));
    }

    #[test]
    fn test_drop_early_max_session_id_ok() {
        let mut params = make_params("ls");
        params.session_id = "a".repeat(MAX_SESSION_ID_LEN);
        assert!(!should_drop_early(&params));
    }

    #[test]
    fn test_drop_early_oversized_command() {
        let mut params = make_params("ls");
        params.command = "x".repeat(MAX_COMMAND_LEN + 1);
        assert!(should_drop_early(&params));
    }

    #[test]
    fn test_drop_early_oversized_cwd() {
        let mut params = make_params("ls");
        params.cwd = "/".repeat(MAX_CWD_LEN + 1);
        assert!(should_drop_early(&params));
    }

    #[test]
    fn test_drop_early_invalid_session_id_chars() {
        let mut params = make_params("ls");
        params.session_id = "bad session/id".to_string();
        assert!(should_drop_early(&params));
    }

    #[test]
    fn test_drop_early_empty_session_id() {
        let mut params = make_params("ls");
        params.session_id = String::new();
        assert!(should_drop_early(&params));
    }

    // ── handle_add_inner end-to-end tests ─────────────────────

    fn default_config() -> config::Config {
        config::Config::default()
    }

    #[test]
    fn test_add_inner_happy_path() {
        let (_dir, repo) = test_repo();
        let cfg = default_config();

        handle_add_inner(make_params("cargo build"), &cfg, &repo).unwrap();

        let entries = repo
            .get_entries_filtered(10, 0, &crate::repository::QueryFilter::default())
            .unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].command, "cargo build");
        assert_eq!(entries[0].cwd, "/tmp");
        assert_eq!(entries[0].exit_code, Some(0));
    }

    #[test]
    fn test_add_inner_disabled_config_drops() {
        let (_dir, repo) = test_repo();
        let cfg = config::Config {
            enabled: false,
            ..default_config()
        };

        handle_add_inner(make_params("cargo build"), &cfg, &repo).unwrap();

        let entries = repo
            .get_entries_filtered(10, 0, &crate::repository::QueryFilter::default())
            .unwrap();
        assert_eq!(entries.len(), 0, "disabled config should drop the entry");
    }

    #[test]
    fn test_add_inner_exclusion_drops() {
        let (_dir, repo) = test_repo();
        let cfg = config::Config {
            exclusions: vec!["^ls".to_string()],
            ..default_config()
        };

        // Excluded command
        handle_add_inner(make_params("ls -la"), &cfg, &repo).unwrap();
        // Non-excluded command
        handle_add_inner(make_params("cargo test"), &cfg, &repo).unwrap();

        let entries = repo
            .get_entries_filtered(10, 0, &crate::repository::QueryFilter::default())
            .unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].command, "cargo test");
    }

    #[test]
    fn test_add_inner_creates_session_automatically() {
        let (_dir, repo) = test_repo();
        let cfg = default_config();

        // No session exists yet
        assert!(repo.get_session("test-session-abc").unwrap().is_none());

        handle_add_inner(make_params("echo hello"), &cfg, &repo).unwrap();

        // Session should now exist
        let session = repo.get_session("test-session-abc").unwrap();
        assert!(session.is_some());
        assert_eq!(session.unwrap().created_at, 1_709_683_200_000);
    }

    #[test]
    fn test_add_inner_reuses_existing_session() {
        let (_dir, repo) = test_repo();
        let cfg = default_config();

        // Pre-create the session
        let session = Session {
            id: "test-session-abc".to_string(),
            hostname: "pre-existing".to_string(),
            created_at: 1_000_000,
            tag_id: None,
        };
        repo.insert_session(&session).unwrap();

        handle_add_inner(make_params("echo hello"), &cfg, &repo).unwrap();

        // Session should retain its original hostname/created_at
        let s = repo.get_session("test-session-abc").unwrap().unwrap();
        assert_eq!(s.hostname, "pre-existing");
        assert_eq!(s.created_at, 1_000_000);
    }

    #[test]
    fn test_add_inner_normalizes_second_timestamps() {
        let (_dir, repo) = test_repo();
        let cfg = default_config();

        let mut params = make_params("echo ts");
        params.started_at = 1_709_683_200; // seconds
        params.ended_at = 1_709_683_205; // seconds

        handle_add_inner(params, &cfg, &repo).unwrap();

        let entries = repo
            .get_entries_filtered(1, 0, &crate::repository::QueryFilter::default())
            .unwrap();
        assert_eq!(entries[0].started_at, 1_709_683_200_000);
        assert_eq!(entries[0].duration_ms, 5_000);
    }

    #[test]
    fn test_add_inner_normalizes_nanosecond_timestamps() {
        let (_dir, repo) = test_repo();
        let cfg = default_config();

        let mut params = make_params("echo ns");
        params.started_at = 1_709_683_200_000_000_000; // nanoseconds
        params.ended_at = 1_709_683_200_050_000_000; // 50ms later

        handle_add_inner(params, &cfg, &repo).unwrap();

        let entries = repo
            .get_entries_filtered(1, 0, &crate::repository::QueryFilter::default())
            .unwrap();
        assert_eq!(entries[0].started_at, 1_709_683_200_000);
        assert_eq!(entries[0].duration_ms, 50);
    }

    #[test]
    fn test_add_inner_zero_started_at_uses_ended_at() {
        let (_dir, repo) = test_repo();
        let cfg = default_config();

        let mut params = make_params("echo zero");
        params.started_at = 0;
        params.ended_at = 1_709_683_200_000;

        handle_add_inner(params, &cfg, &repo).unwrap();

        let entries = repo
            .get_entries_filtered(1, 0, &crate::repository::QueryFilter::default())
            .unwrap();
        assert_eq!(entries[0].started_at, 1_709_683_200_000);
    }

    #[test]
    fn test_add_inner_zero_ended_at_uses_started_at() {
        let (_dir, repo) = test_repo();
        let cfg = default_config();

        let mut params = make_params("echo zero-end");
        params.started_at = 1_709_683_200_000;
        params.ended_at = 0;

        handle_add_inner(params, &cfg, &repo).unwrap();

        let entries = repo
            .get_entries_filtered(1, 0, &crate::repository::QueryFilter::default())
            .unwrap();
        assert_eq!(entries[0].started_at, 1_709_683_200_000);
        assert_eq!(entries[0].ended_at, 1_709_683_200_000);
        assert_eq!(entries[0].duration_ms, 0);
    }

    #[test]
    fn test_add_inner_redacts_secrets_by_default() {
        let (_dir, repo) = test_repo();
        let cfg = default_config();

        // Bearer token pattern that should be redacted
        let mut params = make_params("curl -H 'Authorization: Bearer sk-abc12345678901234567890'");
        params.session_id = "redact-session".to_string();

        handle_add_inner(params, &cfg, &repo).unwrap();

        let entries = repo
            .get_entries_filtered(1, 0, &crate::repository::QueryFilter::default())
            .unwrap();
        // The command should have secrets redacted
        assert!(
            !entries[0].command.contains("sk-abc1234567890"),
            "secret should be redacted, got: {}",
            entries[0].command
        );
        assert!(entries[0].command.contains("***REDACTED***"));
    }

    #[test]
    fn test_add_inner_skips_redaction_when_disabled() {
        let (_dir, repo) = test_repo();
        let cfg = config::Config {
            redaction: config::RedactionConfig { enabled: false },
            ..default_config()
        };

        let mut params = make_params("curl -H 'Authorization: Bearer sk-abc12345678901234567890'");
        params.session_id = "no-redact-session".to_string();

        handle_add_inner(params, &cfg, &repo).unwrap();

        let entries = repo
            .get_entries_filtered(1, 0, &crate::repository::QueryFilter::default())
            .unwrap();
        assert!(
            entries[0].command.contains("sk-abc1234567890"),
            "secret should NOT be redacted when disabled"
        );
    }

    #[test]
    fn test_add_inner_preserves_executor_info() {
        let (_dir, repo) = test_repo();
        let cfg = default_config();

        let mut params = make_params("npm test");
        params.session_id = "exec-session".to_string();
        params.executor_type = Some("agent".to_string());
        params.executor = Some("claude-code".to_string());

        handle_add_inner(params, &cfg, &repo).unwrap();

        let entries = repo
            .get_entries_filtered(1, 0, &crate::repository::QueryFilter::default())
            .unwrap();
        assert_eq!(entries[0].executor_type.as_deref(), Some("agent"));
        assert_eq!(entries[0].executor.as_deref(), Some("claude-code"));
    }

    #[test]
    fn test_add_inner_preserves_exit_code() {
        let (_dir, repo) = test_repo();
        let cfg = default_config();

        let mut params = make_params("false");
        params.exit_code = Some(1);
        handle_add_inner(params, &cfg, &repo).unwrap();

        let mut params2 = make_params("true");
        params2.session_id = "session-2".to_string();
        params2.exit_code = None;
        handle_add_inner(params2, &cfg, &repo).unwrap();

        let entries = repo
            .get_entries_filtered(10, 0, &crate::repository::QueryFilter::default())
            .unwrap();
        // Most recent first
        let by_cmd: std::collections::HashMap<&str, &Entry> =
            entries.iter().map(|e| (e.command.as_str(), e)).collect();
        assert_eq!(by_cmd["false"].exit_code, Some(1));
        assert_eq!(by_cmd["true"].exit_code, None);
    }

    #[test]
    fn test_add_inner_auto_tags_by_cwd() {
        let (_dir, repo) = test_repo();
        let mut auto_tags = std::collections::HashMap::new();
        // auto_tags: key = path_prefix, value = tag_name
        auto_tags.insert("/home/user/work".to_string(), "work".to_string());

        let cfg = config::Config {
            auto_tags,
            ..default_config()
        };

        // Command in matching directory
        let mut params = make_params("cargo build");
        params.session_id = "tag-session".to_string();
        params.cwd = "/home/user/work/project".to_string();

        handle_add_inner(params, &cfg, &repo).unwrap();

        let entries = repo
            .get_entries_filtered(1, 0, &crate::repository::QueryFilter::default())
            .unwrap();
        // Tag should be auto-created and assigned
        assert!(
            entries[0].tag_id.is_some(),
            "auto-tag should be assigned for matching cwd"
        );
        assert_eq!(entries[0].tag_name.as_deref(), Some("work"));
    }

    #[test]
    fn test_add_inner_no_auto_tag_for_unmatched_cwd() {
        let (_dir, repo) = test_repo();
        let mut auto_tags = std::collections::HashMap::new();
        auto_tags.insert("/home/user/work".to_string(), "work".to_string());

        let cfg = config::Config {
            auto_tags,
            ..default_config()
        };

        let mut params = make_params("cargo build");
        params.session_id = "no-tag-session".to_string();
        params.cwd = "/home/user/personal".to_string();

        handle_add_inner(params, &cfg, &repo).unwrap();

        let entries = repo
            .get_entries_filtered(1, 0, &crate::repository::QueryFilter::default())
            .unwrap();
        assert!(entries[0].tag_id.is_none(), "no tag for unmatched cwd");
    }

    #[test]
    fn test_add_inner_multiple_entries_same_session() {
        let (_dir, repo) = test_repo();
        let cfg = default_config();

        for i in 0..5 {
            let mut params = make_params(&format!("cmd-{i}"));
            params.started_at = 1_709_683_200_000 + i64::from(i) * 1000;
            params.ended_at = params.started_at + 100;
            handle_add_inner(params, &cfg, &repo).unwrap();
        }

        let entries = repo
            .get_entries_filtered(10, 0, &crate::repository::QueryFilter::default())
            .unwrap();
        assert_eq!(entries.len(), 5);

        // Session should only be created once
        let session = repo.get_session("test-session-abc").unwrap();
        assert!(session.is_some());
    }

    #[test]
    fn test_add_inner_multiple_exclusion_patterns() {
        let (_dir, repo) = test_repo();
        let cfg = config::Config {
            exclusions: vec![
                "^ls".to_string(),
                "password".to_string(),
                "^cd ".to_string(),
            ],
            ..default_config()
        };

        handle_add_inner(make_params("ls -la"), &cfg, &repo).unwrap();
        handle_add_inner(
            {
                let mut p = make_params("echo password=secret");
                p.session_id = "s2".to_string();
                p
            },
            &cfg,
            &repo,
        )
        .unwrap();
        handle_add_inner(
            {
                let mut p = make_params("cd /tmp");
                p.session_id = "s3".to_string();
                p
            },
            &cfg,
            &repo,
        )
        .unwrap();
        handle_add_inner(
            {
                let mut p = make_params("cargo test");
                p.session_id = "s4".to_string();
                p
            },
            &cfg,
            &repo,
        )
        .unwrap();

        let entries = repo
            .get_entries_filtered(10, 0, &crate::repository::QueryFilter::default())
            .unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].command, "cargo test");
    }

    #[test]
    fn test_add_inner_preserves_context() {
        let (_dir, repo) = test_repo();
        let cfg = default_config();

        let mut ctx = std::collections::HashMap::new();
        ctx.insert("shell".to_string(), "zsh".to_string());
        ctx.insert("prompt_id".to_string(), "abc123".to_string());

        let mut params = make_params("git status");
        params.context = Some(ctx);
        params.session_id = "ctx-session".to_string();

        handle_add_inner(params, &cfg, &repo).unwrap();

        let entries = repo
            .get_entries_filtered(1, 0, &crate::repository::QueryFilter::default())
            .unwrap();
        let ctx = entries[0].context.as_ref().unwrap();
        assert_eq!(ctx.get("shell").unwrap(), "zsh");
        assert_eq!(ctx.get("prompt_id").unwrap(), "abc123");
    }
}
