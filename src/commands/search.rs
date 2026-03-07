use std::process;

use crate::config;
use crate::repository::Repository;
use crate::search;

pub struct SearchParams<'a> {
    pub query: Option<&'a String>,
    pub unique: bool,
    pub after: Option<&'a str>,
    pub before: Option<&'a str>,
    pub tag: Option<&'a str>,
    pub exit_code: Option<i32>,
    pub executor: Option<&'a str>,
    pub here: bool,
    pub field: &'a str,
}

pub fn handle_search(p: &SearchParams) -> Result<(), Box<dyn std::error::Error>> {
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

#[cfg(test)]
mod tests {
    /// The exit code used to signal the shell widget that suvadu is
    /// inactive and the shell should fall back to its native search.
    const EXIT_CODE_SHELL_FALLBACK: i32 = 10;

    #[test]
    fn test_shell_fallback_exit_code_is_documented() {
        // This test exists to document the magic exit code.
        // If the value needs to change, update both the constant
        // and the process::exit(10) call in handle_search().
        assert_eq!(EXIT_CODE_SHELL_FALLBACK, 10);
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
}
