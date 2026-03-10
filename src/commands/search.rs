use std::process;

use crate::config;
use crate::models::SearchField;
use crate::repository::Repository;
use crate::search;

/// Exit code signalling the shell widget that suvadu is inactive and should
/// fall back to native search (e.g. Ctrl-R).
const EXIT_CODE_SHELL_FALLBACK: i32 = 10;

pub struct SearchParams<'a> {
    pub query: Option<&'a String>,
    pub unique: bool,
    pub after: Option<&'a str>,
    pub before: Option<&'a str>,
    pub tag: Option<&'a str>,
    pub exit_code: Option<i32>,
    pub executor: Option<&'a str>,
    pub here: bool,
    pub field: SearchField,
}

pub fn handle_search(p: &SearchParams) -> Result<(), Box<dyn std::error::Error>> {
    // Check if recording is enabled/active
    // If not, we want to fallback to the shell's default search.
    // We use exit code 10 to signal this to the shell widget.
    if !config::should_record()? {
        process::exit(EXIT_CODE_SHELL_FALLBACK);
    }

    // Initialize database
    let repo = Repository::init()?;
    let app_config = config::load_config()?;

    // Auto-filter by session tag if enabled and no explicit tag provided
    let mut resolved_tag = p.tag.map(String::from);
    if p.tag.is_none() && app_config.search.filter_by_current_session_tag {
        if let Ok(session_id) = std::env::var("SUVADU_SESSION_ID") {
            if let Ok(Some(current_tag)) = repo.get_tag_by_session(&session_id) {
                resolved_tag = Some(current_tag);
            }
        }
    }

    // Resolve --here flag to current directory path
    let cwd_filter = if p.here {
        Some(std::env::current_dir()?.to_string_lossy().to_string())
    } else {
        None
    };

    // Run TUI
    let selected = search::run_search(
        &repo,
        &search::SearchArgs {
            initial_query: p.query.map(String::as_str),
            unique_mode: p.unique,
            after: p.after,
            before: p.before,
            tag: resolved_tag.as_deref(),
            exit_code: p.exit_code,
            executor: p.executor,
            prefix_match: false,
            cwd: cwd_filter.as_deref(),
            field: p.field,
        },
    )?;

    // Output selected command to stdout (for shell to execute)
    if let Some(cmd) = selected {
        println!("{cmd}");
    }

    Ok(())
}

pub fn handle_get(
    query: &str,
    offset: usize,
    prefix: bool,
    cwd: Option<&str>,
) -> Result<(), Box<dyn std::error::Error>> {
    let repo = Repository::init()?;

    // When CWD is provided and context_boost is enabled, use recency-based
    // ranking that prefers same-directory commands. Otherwise fall back to
    // plain recency.
    let config = config::load_config().unwrap_or_default();
    let boost_cwd = if config.search.context_boost {
        cwd
    } else {
        None
    };

    if let Some(cmd) = get_from_repo(&repo, query, offset, prefix, boost_cwd)? {
        print!("{cmd}");
    }

    Ok(())
}

/// Core logic for `handle_get`: query the repository and return the matching
/// command string, if any.
fn get_from_repo(
    repo: &Repository,
    query: &str,
    offset: usize,
    prefix: bool,
    boost_cwd: Option<&str>,
) -> Result<Option<String>, Box<dyn std::error::Error>> {
    let query_opt = if query.is_empty() { None } else { Some(query) };
    let results = repo.get_recent_entries(1, offset, query_opt, prefix, boost_cwd)?;
    Ok(results.into_iter().next().map(|e| e.command))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::{Entry, Session};
    use crate::test_utils::test_repo;

    fn seed_session(repo: &Repository, session_id: &str) {
        let session = Session {
            id: session_id.to_string(),
            hostname: "test-host".to_string(),
            created_at: 1_700_000_000_000,
            tag_id: None,
        };
        repo.insert_session(&session).unwrap();
    }

    fn seed_entry(repo: &Repository, session_id: &str, cmd: &str, started_at: i64) {
        let entry = Entry::new(
            session_id.to_string(),
            cmd.to_string(),
            "/tmp".to_string(),
            Some(0),
            started_at,
            started_at + 100,
        );
        repo.insert_entry(&entry).unwrap();
    }

    #[test]
    fn test_shell_fallback_exit_code_is_documented() {
        // This test exists to document the exit code constant.
        assert_eq!(super::EXIT_CODE_SHELL_FALLBACK, 10);
    }

    #[test]
    fn test_empty_query_becomes_none() {
        // Verify the logic: empty string → None, non-empty → Some
        let query = "";
        let query_opt = if query.is_empty() { None } else { Some(query) };
        assert!(query_opt.is_none());

        let query = "git";
        let query_opt = if query.is_empty() { None } else { Some(query) };
        assert_eq!(query_opt, Some("git"));
    }

    #[test]
    fn test_handle_get_returns_most_recent() {
        let (_dir, repo) = test_repo();
        seed_session(&repo, "s1");
        seed_entry(&repo, "s1", "echo old", 1_000);
        seed_entry(&repo, "s1", "echo new", 2_000);
        seed_entry(&repo, "s1", "echo newest", 3_000);

        // With no query, offset 0 returns the most recent entry
        let result = get_from_repo(&repo, "", 0, false, None).unwrap();
        assert_eq!(result.as_deref(), Some("echo newest"));

        // Offset 1 returns the second most recent
        let result = get_from_repo(&repo, "", 1, false, None).unwrap();
        assert_eq!(result.as_deref(), Some("echo new"));

        // Offset 2 returns the oldest
        let result = get_from_repo(&repo, "", 2, false, None).unwrap();
        assert_eq!(result.as_deref(), Some("echo old"));
    }

    #[test]
    fn test_handle_get_with_query_filter() {
        let (_dir, repo) = test_repo();
        seed_session(&repo, "s1");
        seed_entry(&repo, "s1", "git status", 1_000);
        seed_entry(&repo, "s1", "docker ps", 2_000);
        seed_entry(&repo, "s1", "git log", 3_000);

        // Query "git" should match git commands, most recent first
        let result = get_from_repo(&repo, "git", 0, false, None).unwrap();
        assert_eq!(result.as_deref(), Some("git log"));

        let result = get_from_repo(&repo, "git", 1, false, None).unwrap();
        assert_eq!(result.as_deref(), Some("git status"));

        // Query "docker" should match only the docker command
        let result = get_from_repo(&repo, "docker", 0, false, None).unwrap();
        assert_eq!(result.as_deref(), Some("docker ps"));

        // No second docker match
        let result = get_from_repo(&repo, "docker", 1, false, None).unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn test_handle_get_empty_db() {
        let (_dir, repo) = test_repo();

        // No entries at all
        let result = get_from_repo(&repo, "", 0, false, None).unwrap();
        assert!(result.is_none());

        // With a query on empty DB
        let result = get_from_repo(&repo, "git", 0, false, None).unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn test_handle_get_prefix_match() {
        let (_dir, repo) = test_repo();
        seed_session(&repo, "s1");
        seed_entry(&repo, "s1", "git status", 1_000);
        seed_entry(&repo, "s1", "git log --oneline", 2_000);
        seed_entry(&repo, "s1", "docker ps", 3_000);

        // Prefix "git" should match both git commands
        let result = get_from_repo(&repo, "git", 0, true, None).unwrap();
        assert_eq!(result.as_deref(), Some("git log --oneline"));

        let result = get_from_repo(&repo, "git", 1, true, None).unwrap();
        assert_eq!(result.as_deref(), Some("git status"));

        // Prefix "docker" should match just docker ps
        let result = get_from_repo(&repo, "docker", 0, true, None).unwrap();
        assert_eq!(result.as_deref(), Some("docker ps"));

        // Prefix "xyz" matches nothing
        let result = get_from_repo(&repo, "xyz", 0, true, None).unwrap();
        assert!(result.is_none());
    }
}
