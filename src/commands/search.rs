use std::process;

use crate::config;
use crate::repository::Repository;
use crate::search;

#[allow(clippy::too_many_arguments)]
pub fn handle_search(
    query: Option<&String>,
    unique: bool,
    after: Option<&str>,
    before: Option<&str>,
    tag: Option<&str>,
    exit_code: Option<i32>,
    executor: Option<&str>,
    here: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    // Check if recording is enabled/active
    // If not, we want to fallback to the shell's default search.
    // We use exit code 10 to signal this to the shell widget.
    if !config::should_record()? {
        process::exit(10);
    }

    // Initialize database
    let repo = Repository::init()?;
    let app_config = config::load_config()?;

    // Auto-filter by session tag if enabled and no explicit tag provided
    let mut resolved_tag = tag.map(String::from);
    if tag.is_none() && app_config.search.filter_by_current_session_tag {
        if let Ok(session_id) = std::env::var("SUVADU_SESSION_ID") {
            if let Ok(Some(current_tag)) = repo.get_tag_by_session(&session_id) {
                resolved_tag = Some(current_tag);
            }
        }
    }

    // Resolve --here flag to current directory path
    let cwd_filter = if here {
        Some(std::env::current_dir()?.to_string_lossy().to_string())
    } else {
        None
    };

    // Run TUI
    let selected = search::run_search(
        &repo,
        query.map(String::as_str),
        unique,
        after,
        before,
        resolved_tag.as_deref(),
        exit_code,
        executor,
        false, // TUI uses substring matching
        cwd_filter.as_deref(),
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

    let query_opt = if query.is_empty() { None } else { Some(query) };

    // When CWD is provided and context_boost is enabled, use recency-based
    // ranking that prefers same-directory commands. Otherwise fall back to
    // plain recency.
    let config = config::load_config().unwrap_or_default();
    let boost_cwd = if config.search.context_boost {
        cwd
    } else {
        None
    };

    let results = repo.get_recent_entries(1, offset, query_opt, prefix, boost_cwd)?;

    if let Some(entry) = results.first() {
        print!("{}", entry.command);
    } // Else print nothing

    Ok(())
}
